//! Format brut d'une ligne de transcript Claude Code (`~/.claude/projects/**/*.jsonl`).
//!
//! Seuls les champs utiles sont déclarés ; serde ignore les autres. Les gros
//! sous-documents (`message`, `content`, `input`) restent en `RawValue` et ne
//! sont décodés que pour les lignes qui en ont besoin.

use serde::Deserialize;
use serde_json::value::RawValue;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawLine {
    #[serde(rename = "type", default)]
    pub kind: String,
    pub session_id: Option<String>,
    pub uuid: Option<String>,
    pub timestamp: Option<String>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub version: Option<String>,
    pub agent_id: Option<String>,
    #[serde(default)]
    pub is_sidechain: bool,
    #[serde(default)]
    pub is_meta: bool,
    #[serde(default)]
    pub is_compact_summary: bool,
    #[serde(default)]
    pub is_api_error_message: bool,
    pub subtype: Option<String>,
    /// Ligne `turn_duration` : durée du tour, écrite par Claude Code à sa fin.
    pub duration_ms: Option<i64>,
    pub message: Option<Box<RawValue>>,
    pub ai_title: Option<String>,
    /// Titre donné à la main (renommage dans Claude Code) : prime sur `ai_title`.
    pub custom_title: Option<String>,
    pub account_uuid: Option<String>,
    pub organization_uuid: Option<String>,
    #[serde(rename = "totalCostUSD")]
    pub total_cost_usd: Option<f64>,
    #[serde(rename = "totalAPIDuration")]
    pub total_api_duration: Option<f64>,
    pub total_tool_duration: Option<f64>,
    pub total_duration: Option<f64>,
    pub total_lines_added: Option<f64>,
    pub total_lines_removed: Option<f64>,
    pub model_usage: Option<Box<RawValue>>,
}

#[derive(Deserialize)]
pub struct Message {
    pub id: Option<String>,
    pub model: Option<String>,
    pub content: Option<Box<RawValue>>,
    pub usage: Option<Usage>,
    pub stop_reason: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens_details: Option<OutputDetails>,
    #[serde(default)]
    pub cache_creation: Option<CacheCreation>,
}

#[derive(Deserialize, Default)]
pub struct OutputDetails {
    #[serde(default)]
    pub thinking_tokens: Option<u64>,
}

#[derive(Deserialize, Default)]
pub struct CacheCreation {
    #[serde(default)]
    pub ephemeral_5m_input_tokens: Option<u64>,
    #[serde(default)]
    pub ephemeral_1h_input_tokens: Option<u64>,
}

/// Un bloc de contenu. Struct à champs facultatifs plutôt qu'enum étiqueté :
/// un enum étiqueté met son contenu en tampon, ce qui interdit `RawValue`.
#[derive(Deserialize)]
pub struct Block {
    #[serde(rename = "type", default)]
    pub kind: String,
    pub text: Option<String>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub input: Option<Box<RawValue>>,
    pub tool_use_id: Option<String>,
    pub is_error: Option<bool>,
    pub content: Option<Box<RawValue>>,
    /// Bloc `image` : `{type: "base64", media_type, data}`.
    pub source: Option<Box<RawValue>>,
}

/// Le `content` d'un message : une chaîne ou une liste de blocs.
pub enum Content {
    Text(String),
    Blocks(Vec<Block>),
    None,
}

pub fn decode_content(raw: Option<&RawValue>) -> Content {
    let Some(raw) = raw else { return Content::None };
    let s = raw.get().trim_start();
    if s.starts_with('"') {
        serde_json::from_str::<String>(s).map(Content::Text).unwrap_or(Content::None)
    } else if s.starts_with('[') {
        serde_json::from_str::<Vec<Block>>(s).map(Content::Blocks).unwrap_or(Content::None)
    } else {
        Content::None
    }
}

/// Horodatage ISO-8601 UTC (`2026-11-07T22:48:47.435Z`) → millisecondes Unix.
pub fn parse_ts(s: &str) -> Option<i64> {
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, se) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let mut ms = 0;
    if s.as_bytes().get(19) == Some(&b'.') {
        let frac: String = s[20..].chars().take_while(|c| c.is_ascii_digit()).take(3).collect();
        if !frac.is_empty() {
            ms = frac.parse::<i64>().ok()? * 10i64.pow(3 - frac.len() as u32);
        }
    }
    // Jours depuis l'époque (algorithme « days from civil » de H. Hinnant).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((((days * 24 + h) * 60 + mi) * 60 + se) * 1000 + ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ts_epoch_and_fraction() {
        assert_eq!(parse_ts("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_ts("1970-01-02T00:00:01.5Z"), Some(86_401_500));
        assert_eq!(parse_ts("2026-11-07T22:48:47.435Z"), Some(1_794_091_727_435));
        assert_eq!(parse_ts("garbage"), None);
    }

    #[test]
    fn content_forms() {
        let s: Box<RawValue> = serde_json::from_str(r#""hello""#).unwrap();
        assert!(matches!(decode_content(Some(&s)), Content::Text(t) if t == "hello"));
        let b: Box<RawValue> =
            serde_json::from_str(r#"[{"type":"tool_use","id":"x","name":"Bash","input":{"command":"ls"}}]"#).unwrap();
        match decode_content(Some(&b)) {
            Content::Blocks(v) => assert_eq!(v[0].name.as_deref(), Some("Bash")),
            _ => panic!("blocks attendus"),
        }
    }
}
