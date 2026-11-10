//! Messages programmés : gardés en base, envoyés à l'heure par le démon (même
//! interface fermée). Cible : un chat en direct (repli : reprise de sa
//! conversation s'il a été fermé), ou une conversation passée (reprise).

use crate::chat;
use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

#[derive(Deserialize)]
pub struct NewScheduled {
    pub at_ms: i64,
    /// Chat en direct visé (avec sa conversation, en repli).
    pub chat_id: Option<String>,
    /// Conversation à reprendre.
    pub session_id: Option<String>,
    pub text: String,
    pub model: Option<String>,
}

pub fn create(conn: &Connection, n: &NewScheduled, chat_session: Option<String>) -> Result<Value> {
    if n.text.trim().is_empty() {
        anyhow::bail!("The message cannot be scheduled because it is empty.");
    }
    if n.at_ms < now_ms() - 60_000 {
        anyhow::bail!("The message cannot be scheduled because its time is in the past.");
    }
    let session = n.session_id.clone().or(chat_session);
    if n.chat_id.is_none() && session.is_none() {
        anyhow::bail!("The message cannot be scheduled because it names no conversation.");
    }
    conn.execute(
        "INSERT INTO scheduled(at_ms, chat_id, session_id, text, model, status, created) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6)",
        params![n.at_ms, n.chat_id, session, n.text, n.model, now_ms()],
    )?;
    Ok(json!({"id": conn.last_insert_rowid()}))
}

/// Messages en attente d'une conversation (par chat et/ou conversation).
pub fn list(conn: &Connection, chat_id: Option<&str>, session_id: Option<&str>) -> Result<Value> {
    let mut st = conn.prepare(
        "SELECT id, at_ms, text, model FROM scheduled
         WHERE status = 'pending' AND ((?1 IS NOT NULL AND chat_id = ?1) OR (?2 IS NOT NULL AND session_id = ?2))
         ORDER BY at_ms",
    )?;
    let rows: Vec<Value> = st
        .query_map(params![chat_id, session_id], |r| {
            Ok(json!({"id": r.get::<_, i64>(0)?, "at_ms": r.get::<_, i64>(1)?, "text": r.get::<_, String>(2)?, "model": r.get::<_, Option<String>>(3)?}))
        })?
        .collect::<Result<_, _>>()?;
    Ok(Value::Array(rows))
}

pub fn cancel(conn: &Connection, id: i64) -> Result<()> {
    let n = conn.execute("UPDATE scheduled SET status = 'cancelled' WHERE id = ?1 AND status = 'pending'", params![id])?;
    if n == 0 {
        anyhow::bail!("The scheduled message cannot be cancelled because it was already sent or cancelled.");
    }
    Ok(())
}

struct Due {
    id: i64,
    chat_id: Option<String>,
    session_id: Option<String>,
    text: String,
    model: Option<String>,
}

/// Boucle du démon : toutes les 10 s, envoie ce qui est échu.
pub fn spawn(db: Arc<Mutex<Connection>>, hub: chat::Hub, spec_for_resume: fn(&Connection, &str, Option<String>) -> Result<chat::LaunchSpec>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(10));
        loop {
            tick.tick().await;
            let due: Vec<Due> = {
                let Ok(c) = db.lock() else { continue };
                let Ok(mut st) = c.prepare("SELECT id, chat_id, session_id, text, model FROM scheduled WHERE status = 'pending' AND at_ms <= ?1 ORDER BY at_ms") else { continue };
                st.query_map(params![now_ms()], |r| Ok(Due { id: r.get(0)?, chat_id: r.get(1)?, session_id: r.get(2)?, text: r.get(3)?, model: r.get(4)? }))
                    .map(|rows| rows.flatten().collect())
                    .unwrap_or_default()
            };
            for d in due {
                let outcome: Result<String> = async {
                    // Chat encore ouvert : on lui envoie (file d'attente s'il travaille).
                    if let Some(chat) = d.chat_id.as_deref().and_then(|id| hub.get(id).ok()) {
                        if let Some(m) = &d.model {
                            chat.set_model(m).await?;
                        }
                        chat.send(&d.text, vec![]).await?;
                        return Ok(chat.id.clone());
                    }
                    // Sinon : reprise de la conversation dans un nouveau chat.
                    let sid = d.session_id.clone().ok_or_else(|| anyhow::anyhow!("The chat is closed and its conversation is unknown."))?;
                    let spec = {
                        let c = db.lock().map_err(|_| anyhow::anyhow!("database lock poisoned"))?;
                        spec_for_resume(&c, &sid, d.model.clone())?
                    };
                    let chat = hub.start(spec, Some((d.text.clone(), vec![]))).await?;
                    Ok(chat.id.clone())
                }
                .await;
                if let Ok(c) = db.lock() {
                    let _ = match &outcome {
                        Ok(chat_id) => c.execute("UPDATE scheduled SET status = 'sent', sent_at = ?2, result_chat = ?3 WHERE id = ?1", params![d.id, now_ms(), chat_id]),
                        Err(e) => c.execute("UPDATE scheduled SET status = 'failed', sent_at = ?2, error = ?3 WHERE id = ?1", params![d.id, now_ms(), e.to_string()]),
                    };
                }
            }
        }
    });
}
