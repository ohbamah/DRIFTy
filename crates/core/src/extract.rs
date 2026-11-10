//! Transforme les lignes brutes d'UN fichier en enregistrements prêts pour la base.
//! Aucune E/S ici : on reçoit des lignes, on rend un `FileBatch`.

use crate::raw::{self, Content, Message, RawLine};
use std::collections::{HashMap, HashSet};

#[derive(Default, Debug, Clone)]
pub struct SessionAgg {
    pub first_ts: Option<i64>,
    pub last_ts: Option<i64>,
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub version: Option<String>,
    pub title: Option<String>,
    /// Renommage manuel : prime toujours sur le titre automatique.
    pub custom_title: Option<String>,
    pub account: Option<String>,
    pub org: Option<String>,
    pub cost: Option<CostState>,
}

#[derive(Debug, Clone)]
pub struct CostState {
    pub cost_usd: f64,
    pub api_ms: Option<f64>,
    pub tool_ms: Option<f64>,
    pub total_ms: Option<f64>,
    pub lines_added: Option<f64>,
    pub lines_removed: Option<f64>,
    pub model_usage: Option<String>,
}

#[derive(Debug)]
pub struct MsgRec {
    pub id: String,
    pub session_id: String,
    pub agent_id: String,
    pub ts: i64,
    pub model: String,
    pub input: u64,
    pub cache_write: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
    pub output: u64,
    pub thinking: u64,
    pub stop_reason: Option<String>,
}

#[derive(Debug)]
pub struct ToolUseRec {
    pub id: String,
    pub session_id: String,
    pub agent_id: String,
    pub message_id: String,
    pub ts: i64,
    pub name: String,
    pub target: Option<String>,
    pub input_hash: i64,
    pub input_bytes: i64,
}

#[derive(Debug)]
pub struct ToolResultRec {
    pub tool_use_id: String,
    pub ts: i64,
    pub is_error: bool,
    pub bytes: i64,
    pub result_hash: i64,
    pub error_preview: Option<String>,
}

#[derive(Debug)]
pub struct TurnRec {
    pub uuid: String,
    pub session_id: String,
    pub agent_id: String,
    pub ts: i64,
    /// prompt | agent_prompt | command | interrupt | compact | api_error
    pub kind: &'static str,
    pub preview: String,
}

#[derive(Default, Debug)]
pub struct FileBatch {
    pub sessions: HashMap<String, SessionAgg>,
    pub messages: Vec<MsgRec>,
    pub tool_uses: Vec<ToolUseRec>,
    pub tool_results: Vec<ToolResultRec>,
    pub turns: Vec<TurnRec>,
    /// Texte lisible pour la recherche plein texte : (session, horodatage, rôle, texte).
    pub texts: Vec<(String, i64, &'static str, String)>,
    pub lines: u64,
    pub bad_lines: u64,
    seen_msgs: HashSet<String>,
}

const PREVIEW: usize = 400;

fn preview(s: &str) -> String {
    let t = s.trim();
    match t.char_indices().nth(PREVIEW) {
        Some((i, _)) => format!("{}…", &t[..i]),
        None => t.to_string(),
    }
}

/// FNV-1a 64 bits : empreinte stable d'une entrée ou d'un résultat d'outil.
pub fn fnv1a(bytes: &[u8]) -> i64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h as i64
}

/// La « cible » lisible d'un appel d'outil : fichier, commande, motif, URL…
fn tool_target(input: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(input).ok()?;
    for key in ["file_path", "notebook_path", "path", "command", "pattern", "url", "query", "description", "skill"] {
        if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
            let one_line = s.lines().next().unwrap_or("");
            return Some(preview(one_line).chars().take(200).collect());
        }
    }
    None
}

fn result_text(raw: &str) -> String {
    let s = raw.trim_start();
    if s.starts_with('"') {
        return serde_json::from_str::<String>(s).unwrap_or_default();
    }
    if let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(s) {
        return items
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
    }
    String::new()
}

/// Classe un texte d'utilisateur. `None` = bruit (méta, rappels système).
fn classify_user_text(text: &str, sidechain: bool) -> Option<&'static str> {
    let t = text.trim_start();
    if t.starts_with("[Request interrupted") {
        return Some("interrupt");
    }
    if sidechain {
        return Some("agent_prompt");
    }
    if t.starts_with("<command-name>") {
        return Some("command");
    }
    if t.starts_with('<') || t.is_empty() {
        return None;
    }
    Some("prompt")
}

impl FileBatch {
    pub fn push_line(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        self.lines += 1;
        let raw: RawLine = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(_) => {
                self.bad_lines += 1;
                return;
            }
        };
        let Some(session_id) = raw.session_id.clone() else { return };
        let ts = raw.timestamp.as_deref().and_then(raw::parse_ts);
        self.touch_session(&session_id, &raw, ts);

        let agent_id = if raw.is_sidechain { raw.agent_id.clone().unwrap_or_default() } else { String::new() };
        match raw.kind.as_str() {
            "assistant" => self.assistant(&raw, &session_id, &agent_id, ts),
            "user" => self.user(&raw, &session_id, &agent_id, ts),
            "system" => {
                let kind = match raw.subtype.as_deref() {
                    Some("compact_boundary") => "compact",
                    Some("api_error") => "api_error",
                    _ => return,
                };
                if let (Some(uuid), Some(ts)) = (raw.uuid.clone(), ts) {
                    self.turns.push(TurnRec { uuid, session_id, agent_id, ts, kind, preview: String::new() });
                }
            }
            _ => {}
        }
    }

    fn touch_session(&mut self, sid: &str, raw: &RawLine, ts: Option<i64>) {
        let s = self.sessions.entry(sid.to_string()).or_default();
        if let Some(t) = ts {
            s.first_ts = Some(s.first_ts.map_or(t, |f| f.min(t)));
            s.last_ts = Some(s.last_ts.map_or(t, |l| l.max(t)));
        }
        if !raw.is_sidechain {
            // Le dossier de DÉPART : c'est lui qui désigne le projet.
            if s.cwd.is_none() && raw.cwd.is_some() {
                s.cwd = raw.cwd.clone();
            }
            if raw.git_branch.is_some() {
                s.branch = raw.git_branch.clone();
            }
            if raw.version.is_some() {
                s.version = raw.version.clone();
            }
        }
        match raw.kind.as_str() {
            "ai-title" => s.title = raw.ai_title.clone().or(s.title.take()),
            "custom-title" => s.custom_title = raw.custom_title.clone().filter(|t| !t.trim().is_empty()).or(s.custom_title.take()),
            "session-owner" => {
                s.account = raw.account_uuid.clone();
                s.org = raw.organization_uuid.clone();
            }
            "cost-state" => {
                if let Some(c) = raw.total_cost_usd {
                    // L'état de coût est cumulatif : on garde le plus élevé.
                    if s.cost.as_ref().map_or(true, |old| c >= old.cost_usd) {
                        s.cost = Some(CostState {
                            cost_usd: c,
                            api_ms: raw.total_api_duration,
                            tool_ms: raw.total_tool_duration,
                            total_ms: raw.total_duration,
                            lines_added: raw.total_lines_added,
                            lines_removed: raw.total_lines_removed,
                            model_usage: raw.model_usage.as_ref().map(|r| r.get().to_string()),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    fn message(raw: &RawLine) -> Option<Message> {
        serde_json::from_str(raw.message.as_ref()?.get()).ok()
    }

    fn assistant(&mut self, raw: &RawLine, sid: &str, agent: &str, ts: Option<i64>) {
        let Some(ts) = ts else { return };
        let Some(msg) = Self::message(raw) else { return };
        if raw.is_api_error_message {
            let text = match raw::decode_content(msg.content.as_deref()) {
                Content::Blocks(b) => b.into_iter().filter_map(|b| b.text).collect::<Vec<_>>().join(" "),
                Content::Text(t) => t,
                Content::None => String::new(),
            };
            if let Some(uuid) = raw.uuid.clone() {
                self.turns.push(TurnRec {
                    uuid,
                    session_id: sid.into(),
                    agent_id: agent.into(),
                    ts,
                    kind: "api_error",
                    preview: preview(&text),
                });
            }
            return;
        }
        let Some(id) = msg.id.clone() else { return };
        // Un message de l'API est écrit sur plusieurs lignes (une par bloc),
        // chacune avec le MÊME `usage` : on ne le compte qu'une fois.
        if self.seen_msgs.insert(id.clone()) {
            let u = msg.usage.unwrap_or_default();
            let cc = u.cache_creation.unwrap_or_default();
            self.messages.push(MsgRec {
                id: id.clone(),
                session_id: sid.into(),
                agent_id: agent.into(),
                ts,
                model: msg.model.clone().unwrap_or_default(),
                input: u.input_tokens.unwrap_or(0),
                cache_write: u.cache_creation_input_tokens.unwrap_or(0),
                cache_write_5m: cc.ephemeral_5m_input_tokens.unwrap_or(0),
                cache_write_1h: cc.ephemeral_1h_input_tokens.unwrap_or(0),
                cache_read: u.cache_read_input_tokens.unwrap_or(0),
                output: u.output_tokens.unwrap_or(0),
                thinking: u.output_tokens_details.and_then(|d| d.thinking_tokens).unwrap_or(0),
                stop_reason: msg.stop_reason.clone(),
            });
        }
        if let Content::Blocks(blocks) = raw::decode_content(msg.content.as_deref()) {
            for b in blocks {
                // Réponse de l'agent principal : indexée pour la recherche.
                if b.kind == "text" && agent.is_empty() {
                    if let Some(t) = b.text.as_deref().filter(|t| !t.trim().is_empty()) {
                        self.texts.push((sid.into(), ts, "assistant", t.to_string()));
                    }
                    continue;
                }
                if b.kind != "tool_use" {
                    continue;
                }
                let (Some(tid), Some(name)) = (b.id, b.name) else { continue };
                let input = b.input.as_ref().map(|r| r.get()).unwrap_or("{}");
                self.tool_uses.push(ToolUseRec {
                    id: tid,
                    session_id: sid.into(),
                    agent_id: agent.into(),
                    message_id: id.clone(),
                    ts,
                    name,
                    target: tool_target(input),
                    input_hash: fnv1a(input.as_bytes()),
                    input_bytes: input.len() as i64,
                });
            }
        }
    }

    fn user(&mut self, raw: &RawLine, sid: &str, agent: &str, ts: Option<i64>) {
        let Some(ts) = ts else { return };
        let uuid = raw.uuid.clone().unwrap_or_default();
        if raw.is_compact_summary {
            self.turns.push(TurnRec { uuid, session_id: sid.into(), agent_id: agent.into(), ts, kind: "compact", preview: String::new() });
            return;
        }
        let Some(msg) = Self::message(raw) else { return };
        let mut texts = Vec::new();
        match raw::decode_content(msg.content.as_deref()) {
            Content::Text(t) => texts.push(t),
            Content::Blocks(blocks) => {
                for b in blocks {
                    match b.kind.as_str() {
                        "tool_result" => {
                            let Some(tid) = b.tool_use_id else { continue };
                            let body = b.content.as_ref().map(|r| r.get()).unwrap_or("");
                            let is_error = b.is_error.unwrap_or(false);
                            self.tool_results.push(ToolResultRec {
                                tool_use_id: tid,
                                ts,
                                is_error,
                                bytes: body.len() as i64,
                                result_hash: fnv1a(body.as_bytes()),
                                error_preview: is_error.then(|| preview(&result_text(body))),
                            });
                        }
                        "text" => texts.extend(b.text),
                        _ => {}
                    }
                }
            }
            Content::None => {}
        }
        if raw.is_meta {
            return;
        }
        for (i, t) in texts.iter().enumerate() {
            if let Some(kind) = classify_user_text(t, raw.is_sidechain) {
                if kind == "prompt" {
                    self.texts.push((sid.into(), ts, "user", t.clone()));
                }
                let uuid = if i == 0 { uuid.clone() } else { format!("{uuid}#{i}") };
                self.turns.push(TurnRec { uuid, session_id: sid.into(), agent_id: agent.into(), ts, kind, preview: preview(t) });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_split_over_lines_counted_once() {
        let mut b = FileBatch::default();
        let l1 = r#"{"type":"assistant","sessionId":"s","uuid":"a","timestamp":"2026-01-01T00:00:00Z","message":{"id":"m1","model":"claude-opus-5-5","content":[{"type":"thinking","thinking":"…"}],"usage":{"input_tokens":2,"cache_read_input_tokens":100,"output_tokens":5}}}"#;
        let l2 = r#"{"type":"assistant","sessionId":"s","uuid":"b","timestamp":"2026-01-01T00:00:01Z","message":{"id":"m1","model":"claude-opus-5-5","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/x/y.rs"}}],"usage":{"input_tokens":2,"cache_read_input_tokens":100,"output_tokens":5}}}"#;
        let l3 = r#"{"type":"user","sessionId":"s","uuid":"c","timestamp":"2026-01-01T00:00:03Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"boom"}]}}"#;
        for l in [l1, l2, l3] {
            b.push_line(l);
        }
        assert_eq!(b.messages.len(), 1);
        assert_eq!(b.messages[0].cache_read, 100);
        assert_eq!(b.tool_uses.len(), 1);
        assert_eq!(b.tool_uses[0].target.as_deref(), Some("/x/y.rs"));
        assert_eq!(b.tool_results[0].error_preview.as_deref(), Some("boom"));
    }

    #[test]
    fn custom_title_is_kept_apart_from_ai_title() {
        let mut b = FileBatch::default();
        b.push_line(r#"{"type":"ai-title","aiTitle":"Titre auto","sessionId":"s"}"#);
        b.push_line(r#"{"type":"custom-title","customTitle":"Contextator MK3","sessionId":"s"}"#);
        b.push_line(r#"{"type":"ai-title","aiTitle":"Autre titre auto","sessionId":"s"}"#);
        let s = &b.sessions["s"];
        assert_eq!(s.custom_title.as_deref(), Some("Contextator MK3"));
        assert_eq!(s.title.as_deref(), Some("Autre titre auto"));
    }

    #[test]
    fn user_text_classification() {
        assert_eq!(classify_user_text("[Request interrupted by user]", false), Some("interrupt"));
        assert_eq!(classify_user_text("<system-reminder>x", false), None);
        assert_eq!(classify_user_text("<command-name>/clear</command-name>", false), Some("command"));
        assert_eq!(classify_user_text("fais ça", false), Some("prompt"));
        assert_eq!(classify_user_text("<teammate-message>…", true), Some("agent_prompt"));
    }
}
