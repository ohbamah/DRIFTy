//! Lecture d'une conversation depuis son transcript, à la demande (rien de
//! ce texte n'est copié en base).

use crate::raw::{self, Content, RawLine};
use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::io::{BufRead, BufReader};

/// Garde-fou contre un message démesuré (un collage de plusieurs Mo), pas une coupe de lecture :
/// à 6 000, une réponse longue s'affichait tronquée (« … ») et son transfert ou sa copie aussi.
const MAX_TEXT: usize = 200_000;

#[derive(Serialize, Default)]
pub struct Entry {
    /// user | assistant | tool | event | turn (fin d'un tour : `ts` = heure de fin)
    pub role: &'static str,
    pub ts: Option<i64>,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_id: Option<String>,
    /// Images jointes (message utilisateur) : clés `ligne:bloc`, servies à la demande.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// Modification de fichier : [lignes ajoutées, lignes retirées].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<[i64; 2]>,
    /// Fin d'un tour : sa durée totale.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
}

fn clip(s: &str) -> String {
    match s.char_indices().nth(MAX_TEXT) {
        Some((i, _)) => format!("{}\n…", &s[..i]),
        None => s.to_string(),
    }
}

fn tool_label(input: Option<&serde_json::value::RawValue>) -> String {
    let Some(v) = input.and_then(|r| serde_json::from_str::<serde_json::Value>(r.get()).ok()) else { return String::new() };
    for k in ["description", "file_path", "command", "pattern", "url", "query", "prompt", "skill"] {
        if let Some(s) = v.get(k).and_then(|x| x.as_str()) {
            return s.lines().next().unwrap_or("").chars().take(240).collect();
        }
    }
    String::new()
}

pub fn read(conn: &Connection, sid: &str) -> Result<Vec<Entry>> {
    let path: Option<String> = conn.query_row("SELECT file_path FROM sessions WHERE id = ?1", params![sid], |r| r.get(0))?;
    let Some(path) = path else {
        anyhow::bail!("The conversation `{sid}` cannot be read because its transcript file is unknown.");
    };
    let f = std::fs::File::open(&path).map_err(|e| anyhow::anyhow!("The transcript `{path}` cannot be read because {e}."))?;
    let mut out = Vec::new();
    for (li, line) in BufReader::new(f).lines().enumerate() {
        let Ok(line) = line else { continue };
        let Ok(r) = serde_json::from_str::<RawLine>(&line) else { continue };
        if r.is_sidechain || r.is_meta {
            continue;
        }
        let ts = r.timestamp.as_deref().and_then(raw::parse_ts);
        // Fin d'un tour (écrite par Claude Code) : heure de fin et durée totale.
        if r.subtype.as_deref() == Some("turn_duration") {
            out.push(Entry { role: "turn", ts, duration_ms: r.duration_ms, ..Default::default() });
            continue;
        }
        if r.is_compact_summary || r.subtype.as_deref() == Some("compact_boundary") {
            if r.subtype.is_some() {
                out.push(Entry { role: "event", ts, text: "Contexte compacté".into(), tool: None, tool_id: None, images: vec![], diff: None, ..Default::default() });
            }
            continue;
        }
        let Some(msg) = r.message.as_ref().and_then(|m| serde_json::from_str::<raw::Message>(m.get()).ok()) else { continue };
        match (r.kind.as_str(), raw::decode_content(msg.content.as_deref())) {
            ("user", Content::Text(t)) => push_user(&mut out, ts, &t),
            ("user", Content::Blocks(bs)) => {
                let before = out.len();
                let mut images = Vec::new();
                for (bi, b) in bs.into_iter().enumerate() {
                    match b.kind.as_str() {
                        "text" => {
                            if let Some(t) = b.text {
                                push_user(&mut out, ts, &t);
                            }
                        }
                        "image" => images.push(format!("{li}:{bi}")),
                        _ => {}
                    }
                }
                // Les images vont avec le texte du même message ; sans texte, elles forment leur propre message.
                if !images.is_empty() {
                    match out[before..].iter_mut().rev().find(|e| e.role == "user") {
                        Some(e) => e.images = images,
                        None => out.push(Entry { role: "user", ts, text: String::new(), tool: None, tool_id: None, images, diff: None, ..Default::default() }),
                    }
                }
            }
            ("assistant", Content::Blocks(bs)) => {
                for b in bs {
                    match b.kind.as_str() {
                        "text" => {
                            if let Some(t) = b.text.filter(|t| !t.trim().is_empty()) {
                                out.push(Entry { role: "assistant", ts, text: clip(&t), tool: None, tool_id: None, images: vec![], diff: None, ..Default::default() });
                            }
                        }
                        "tool_use" => out.push(Entry {
                            duration_ms: None,
                            role: "tool",
                            ts,
                            diff: b
                                .input
                                .as_deref()
                                .and_then(|r| serde_json::from_str::<serde_json::Value>(r.get()).ok())
                                .and_then(|v| crate::diffstat::for_tool(b.name.as_deref().unwrap_or(""), &v))
                                .map(|(a, r)| [a, r]),
                            text: tool_label(b.input.as_deref()),
                            tool: b.name,
                            tool_id: b.id,
                            images: vec![],
                        }),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

fn push_user(out: &mut Vec<Entry>, ts: Option<i64>, t: &str) {
    let s = t.trim();
    if s.starts_with("[Request interrupted") {
        out.push(Entry { role: "event", ts, text: "Interrompu par l'utilisateur".into(), tool: None, tool_id: None, images: vec![], diff: None, ..Default::default() });
    } else if let Some(cmd) = s.strip_prefix("<command-name>") {
        let name = cmd.split("</command-name>").next().unwrap_or("");
        out.push(Entry { role: "event", ts, text: format!("Commande {name}"), tool: None, tool_id: None, images: vec![], diff: None, ..Default::default() });
    } else if !s.is_empty() && !s.starts_with('<') {
        out.push(Entry { role: "user", ts, text: clip(s), tool: None, tool_id: None, images: vec![], diff: None, ..Default::default() });
    }
}

/// Une image jointe d'une conversation, d'après sa clé `ligne:bloc` : (type MIME, octets).
pub fn image(conn: &Connection, sid: &str, key: &str) -> Result<(String, Vec<u8>)> {
    let (li, bi) = key
        .split_once(':')
        .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?)))
        .ok_or_else(|| anyhow::anyhow!("The image `{key}` cannot be read because its key is not `line:block`."))?;
    let path: Option<String> = conn.query_row("SELECT file_path FROM sessions WHERE id = ?1", params![sid], |r| r.get(0))?;
    let path = path.ok_or_else(|| anyhow::anyhow!("The image cannot be read because the transcript file is unknown."))?;
    let line = BufReader::new(std::fs::File::open(&path)?)
        .lines()
        .nth(li)
        .ok_or_else(|| anyhow::anyhow!("The image cannot be read because the transcript has no line {li}."))??;
    let r: RawLine = serde_json::from_str(&line)?;
    let msg: raw::Message = serde_json::from_str(r.message.as_ref().map(|m| m.get()).unwrap_or("null"))?;
    let Content::Blocks(bs) = raw::decode_content(msg.content.as_deref()) else {
        anyhow::bail!("The image cannot be read because the message has no blocks.");
    };
    let src = bs
        .get(bi)
        .and_then(|b| b.source.as_ref())
        .ok_or_else(|| anyhow::anyhow!("The image cannot be read because block {bi} is not an image."))?;
    decode_image(src.get())
}

/// `{type: "base64", media_type, data}` → (type MIME, octets). Seuls les formats d'image web sont servis.
pub fn decode_image(source_json: &str) -> Result<(String, Vec<u8>)> {
    use base64::Engine;
    let v: serde_json::Value = serde_json::from_str(source_json)?;
    let media = v["media_type"].as_str().unwrap_or("").to_string();
    if !matches!(media.as_str(), "image/png" | "image/jpeg" | "image/gif" | "image/webp") {
        anyhow::bail!("The image cannot be shown because its type `{media}` is not a web image format.");
    }
    let data = v["data"].as_str().ok_or_else(|| anyhow::anyhow!("The image cannot be shown because it is not embedded data."))?;
    Ok((media, base64::engine::general_purpose::STANDARD.decode(data)?))
}

#[cfg(test)]
mod clip_tests {
    #[test]
    fn a_long_reply_is_not_cut() {
        let reply = "é".repeat(17_889);
        assert_eq!(super::clip(&reply), reply);
        assert!(super::clip(&"x".repeat(super::MAX_TEXT + 1)).ends_with("\n…"));
    }
}
