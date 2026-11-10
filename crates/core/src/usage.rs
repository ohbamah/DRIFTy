//! Usage d'abonnement (fenêtre de 5 h, semaine) de chaque compte.
//!
//! Source officielle : le JSON que Claude Code envoie à son script de
//! statusline (`rate_limits.five_hour` / `rate_limits.seven_day`). Le relais
//! `driftyd statusline --slot <compte>` en écrit le dernier relevé dans
//! `~/.drifty/usage/<compte>.json`. Rien n'est demandé à un service.

use crate::db;
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub fn dir() -> PathBuf {
    crate::db_path().parent().map(Path::to_path_buf).unwrap_or_else(|| crate::home().join(".drifty")).join("usage")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// Écrit le relevé si l'entrée de la statusline en porte un. Écriture atomique.
pub fn record(slot: &str, statusline_input: &[u8]) -> Result<bool> {
    if !db::SLOTS.contains(&slot) {
        anyhow::bail!("The account slot `{slot}` cannot be used because only `perso` and `pro` exist.");
    }
    let v: Value = serde_json::from_slice(statusline_input).context("statusline input is not JSON")?;
    let d = dir();
    let path = d.join(format!("{slot}.json"));
    let mut snap: Value = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(|| json!({}));
    // Le relais a tourné : on le note même sans limites (session sans réponse de Claude pour l'instant).
    snap["seen_at"] = json!(now_ms());
    let got = match v.get("rate_limits").filter(|r| r.is_object()) {
        // Limites reçues : une fenêtre ABSENTE est une fenêtre sans usage (Claude Code ne l'envoie pas),
        // on l'écrit donc à null, que la lecture rend à 0 %.
        Some(rl) => {
            snap["at"] = json!(now_ms());
            snap["five_hour"] = rl.get("five_hour").cloned().unwrap_or(Value::Null);
            snap["seven_day"] = rl.get("seven_day").cloned().unwrap_or(Value::Null);
            snap["model"] = v.pointer("/model/display_name").cloned().unwrap_or(Value::Null);
            true
        }
        // Pas encore de limites dans cette session : on garde le dernier relevé tel quel.
        None => false,
    };
    std::fs::create_dir_all(&d)?;
    let tmp = d.join(format!(".{slot}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(&snap)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(got)
}

/// Modèle du dernier relevé d'un compte (gardé quand le relevé vient de `/usage`).
pub fn summary_model(slot: &str) -> Option<String> {
    let snap: Value = serde_json::from_slice(&std::fs::read(dir().join(format!("{slot}.json"))).ok()?).ok()?;
    snap["model"].as_str().map(str::to_string)
}

/// Relevé venant de `claude -p /usage` (commande locale de Claude Code, sans session ni coût) :
/// « Current session: 26% used · resets Nov 10 at 6:19pm (Europe/Paris) » et « Current week
/// (all models): 27% used · resets Nov 11 at 10:59am ». Rendu au format de la statusline, pour
/// passer par le même point d'écriture (`record`). Heures lues dans le fuseau local.
pub fn parse_usage_text(text: &str, now: chrono::DateTime<chrono::Local>) -> Option<Value> {
    let window = |prefix: &str| -> Option<Value> {
        let line = text.lines().find(|l| l.trim_start().starts_with(prefix))?;
        let pct: f64 = line.split('%').next()?.rsplit(|c: char| !c.is_ascii_digit() && c != '.').next()?.parse().ok()?;
        let resets = line.split("resets ").nth(1).and_then(|r| reset_at(r, now));
        Some(json!({"used_percentage": pct, "resets_at": resets}))
    };
    let five = window("Current session")?;
    let week = window("Current week (all models)").or_else(|| window("Current week"));
    Some(json!({"rate_limits": {"five_hour": five, "seven_day": week}}))
}

/// « Nov 10 at 6:19pm (Europe/Paris) » ou « 6:19pm » → instant Unix (s), dans le fuseau local.
fn reset_at(s: &str, now: chrono::DateTime<chrono::Local>) -> Option<i64> {
    use chrono::{Datelike, NaiveDate, NaiveTime, TimeZone};
    let s = s.split(" (").next()?.trim();
    let (date_part, time_part) = match s.split_once(" at ") {
        Some((d, t)) => (Some(d.trim()), t.trim()),
        None => (None, s),
    };
    let t = time_part.to_ascii_lowercase();
    let (pm, hm) = if let Some(x) = t.strip_suffix("pm") { (true, x) } else if let Some(x) = t.strip_suffix("am") { (false, x) } else { return None };
    let (h, m) = match hm.split_once(':') {
        Some((h, m)) => (h.trim().parse::<u32>().ok()?, m.trim().parse::<u32>().ok()?),
        None => (hm.trim().parse::<u32>().ok()?, 0),
    };
    let h = (h % 12) + if pm { 12 } else { 0 };
    let time = NaiveTime::from_hms_opt(h, m, 0)?;
    let date = match date_part {
        Some(d) => {
            let word = d.split_whitespace().next()?.to_ascii_lowercase();
            let mon = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"].iter().position(|x| word.starts_with(x))? as u32 + 1;
            let day: u32 = d.split_whitespace().nth(1)?.trim_end_matches(',').parse().ok()?;
            let mut date = NaiveDate::from_ymd_opt(now.year(), mon, day)?;
            // Une date déjà passée de plus d'un jour : c'est l'année suivante (relevé de fin décembre).
            if date < now.date_naive() - chrono::Days::new(1) {
                date = NaiveDate::from_ymd_opt(now.year() + 1, mon, day)?;
            }
            date
        }
        None => now.date_naive(),
    };
    chrono::Local.from_local_datetime(&date.and_time(time)).earliest().map(|d| d.timestamp())
}

/// Relevé venant du flux d'un chat (`rate_limit_event.rate_limit_info.unifiedWindows`) :
/// `utilization` de 0 à 1, `resetsAt` en secondes. Même fichier que le relais.
pub fn record_unified(slot: &str, windows: &Value) -> Result<()> {
    if !db::SLOTS.contains(&slot) {
        return Ok(());
    }
    let conv = |w: Option<&Value>| -> Value {
        match w {
            Some(w) => json!({
                "used_percentage": w.get("utilization").and_then(Value::as_f64).map(|u| u * 100.0),
                "resets_at": w.get("resetsAt"),
            }),
            None => Value::Null,
        }
    };
    let d = dir();
    let path = d.join(format!("{slot}.json"));
    let mut snap: Value = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(|| json!({}));
    snap["at"] = json!(now_ms());
    snap["seen_at"] = json!(now_ms());
    snap["five_hour"] = conv(windows.get("five_hour"));
    snap["seven_day"] = conv(windows.get("seven_day"));
    std::fs::create_dir_all(&d)?;
    let tmp = d.join(format!(".{slot}.{}.chat.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(&snap)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Une fenêtre dont l'heure de réinitialisation est passée vaut 0 % (Claude Code la retire aussi).
/// `received` : des limites ont déjà été reçues pour ce compte ; une fenêtre absente vaut alors 0 %.
fn window(w: Option<&Value>, now_s: f64, received: bool) -> Value {
    let Some(w) = w.filter(|w| w.is_object()) else {
        return if received { json!({"used_percentage": 0.0, "resets_at_ms": null, "expired": false, "idle": true}) } else { Value::Null };
    };
    let pct = w.get("used_percentage").and_then(Value::as_f64);
    let resets = w.get("resets_at").and_then(Value::as_f64);
    let expired = resets.is_some_and(|r| r <= now_s);
    json!({
        "used_percentage": if expired { Some(0.0) } else { pct },
        "resets_at_ms": resets.map(|r| (r * 1000.0) as i64),
        "expired": expired,
    })
}

const RELAY_MARK: &str = "statusline --slot";

fn settings_path(config_dir: &str) -> PathBuf {
    Path::new(config_dir).join("settings.json")
}

fn current_statusline(config_dir: &str) -> Option<String> {
    let s = std::fs::read_to_string(settings_path(config_dir)).ok()?;
    let v: Value = serde_json::from_str(&s).ok()?;
    v.pointer("/statusLine/command").and_then(Value::as_str).map(str::to_string)
}

pub fn summary(conn: &Connection) -> Result<Value> {
    let now = now_ms();
    let mut out = Vec::new();
    for a in db::accounts(conn)? {
        let path = dir().join(format!("{}.json", a.slot));
        let snap: Option<Value> = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok());
        let now_s = now as f64 / 1000.0;
        let relay = current_statusline(&a.config_dir).is_some_and(|c| c.contains(RELAY_MARK));
        let received = snap.as_ref().is_some_and(|s| s.get("at").is_some());
        // Activité locale (transcripts déjà synchronisés) sur les mêmes fenêtres.
        let local = |since: i64| -> Result<Value> {
            Ok(conn.query_row(
                "SELECT COUNT(DISTINCT m.session_id), COALESCE(SUM(m.output), 0), COALESCE(SUM(m.input + m.cache_write + m.cache_read), 0)
                 FROM messages m JOIN sessions s ON s.id = m.session_id WHERE s.slot = ?1 AND m.ts >= ?2",
                params![a.slot, since],
                |r| Ok(json!({"conversations": r.get::<_, i64>(0)?, "output": r.get::<_, i64>(1)?, "context": r.get::<_, i64>(2)?})),
            )?)
        };
        out.push(json!({
            "slot": a.slot,
            "label": a.label,
            "enabled": a.enabled,
            "config_dir": a.config_dir,
            "relay_installed": relay,
            "snapshot_at": snap.as_ref().and_then(|s| s.get("at")).and_then(Value::as_i64),
            "seen_at": snap.as_ref().and_then(|s| s.get("seen_at")).and_then(Value::as_i64),
            "model": snap.as_ref().and_then(|s| s.get("model")).cloned(),
            "five_hour": window(snap.as_ref().and_then(|s| s.get("five_hour")), now_s, received),
            "seven_day": window(snap.as_ref().and_then(|s| s.get("seven_day")), now_s, received),
            "local_5h": local(now - 5 * 3_600_000)?,
            "local_7d": local(now - 7 * 86_400_000)?,
        }));
    }
    Ok(Value::Array(out))
}

/// Branche le relais sur la statusline du compte. L'ancienne commande est
/// gardée (le relais la rejoue) et `settings.json` est sauvegardé à côté.
pub fn install_relay(conn: &Connection, slot: &str, exe: &Path) -> Result<String> {
    let a = db::accounts(conn)?
        .into_iter()
        .find(|a| a.slot == slot)
        .ok_or_else(|| anyhow::anyhow!("The relay cannot be installed because the `{slot}` account is not configured."))?;
    let path = settings_path(&a.config_dir);
    let text = if path.exists() { std::fs::read_to_string(&path)? } else { "{}".to_string() };
    let mut v: Value = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("The relay cannot be installed because `{}` is not valid JSON ({e}).", path.display()))?;
    let old = v.pointer("/statusLine/command").and_then(Value::as_str).map(str::to_string);
    if old.as_deref().is_some_and(|c| c.contains(RELAY_MARK)) {
        return Ok(old.unwrap_or_default());
    }
    let q = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
    let mut cmd = format!("{} statusline --slot {slot}", q(&exe.to_string_lossy()));
    if let Some(old) = &old {
        cmd += &format!(" -- {}", q(old));
    }
    if path.exists() {
        std::fs::copy(&path, path.with_extension("json.drifty-backup"))?;
    }
    let obj = v.as_object_mut().ok_or_else(|| anyhow::anyhow!("The relay cannot be installed because `{}` is not a JSON object.", path.display()))?;
    let mut sl = obj.get("statusLine").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
    sl["type"] = json!("command");
    sl["command"] = json!(cmd);
    obj.insert("statusLine".into(), sl);
    std::fs::write(&path, serde_json::to_string_pretty(&v)? + "\n")?;
    Ok(cmd)
}

/// Ancien nom (Claudius) : une statusline qui appelle `…/claudiusd statusline` appelle
/// désormais le binaire courant (`exe`). Sauvegarde du réglage avant réécriture.
pub fn migrate_relay_commands(conn: &Connection, exe: &Path) -> Result<usize> {
    let mut n = 0;
    for a in db::accounts(conn)? {
        let path = settings_path(&a.config_dir);
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Ok(mut v) = serde_json::from_str::<Value>(&text) else { continue };
        let Some(cmd) = v.pointer("/statusLine/command").and_then(Value::as_str).map(str::to_string) else { continue };
        let Some(at) = cmd.find("claudiusd' statusline").or_else(|| cmd.find("claudiusd statusline")) else { continue };
        // Le chemin de l'ancien binaire : du début de la commande jusqu'à `claudiusd` (guillemets compris).
        let quoted = cmd.starts_with('\'');
        let end = at + "claudiusd".len() + usize::from(quoted);
        let q = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
        let new = format!("{}{}", q(&exe.to_string_lossy()), &cmd[end..]);
        std::fs::copy(&path, path.with_extension("json.drifty-backup"))?;
        v["statusLine"]["command"] = json!(new);
        std::fs::write(&path, serde_json::to_string_pretty(&v)? + "\n")?;
        n += 1;
    }
    Ok(n)
}

/// Débranche le relais : remet la commande d'origine.
pub fn remove_relay(conn: &Connection, slot: &str) -> Result<()> {
    let a = db::accounts(conn)?
        .into_iter()
        .find(|a| a.slot == slot)
        .ok_or_else(|| anyhow::anyhow!("The relay cannot be removed because the `{slot}` account is not configured."))?;
    let path = settings_path(&a.config_dir);
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    let Some(cmd) = v.pointer("/statusLine/command").and_then(Value::as_str).map(str::to_string) else { return Ok(()) };
    if !cmd.contains(RELAY_MARK) {
        return Ok(());
    }
    let original = cmd.split_once(" -- ").map(|(_, rest)| unquote(rest));
    let obj = v.as_object_mut().unwrap();
    match original {
        Some(o) => {
            obj["statusLine"]["command"] = json!(o);
        }
        None => {
            obj.remove("statusLine");
        }
    }
    std::fs::write(&path, serde_json::to_string_pretty(&v)? + "\n")?;
    Ok(())
}

/// Inverse de la mise entre apostrophes de `install_relay`.
fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        s[1..s.len() - 1].replace(r"'\''", "'")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod usage_text_tests {
    #[test]
    fn usage_command_text_is_parsed() {
        use chrono::TimeZone;
        let now = chrono::Local.with_ymd_and_hms(2026, 11, 10, 16, 0, 0).unwrap();
        let txt = "You are currently using your subscription\n\nCurrent session: 26% used · resets Nov 10 at 6:19pm (Europe/Paris)\nCurrent week (all models): 27% used · resets Nov 11 at 10:59am (Europe/Paris)\nCurrent week (Fable): 0% used · resets Nov 11 at 11am";
        let v = super::parse_usage_text(txt, now).unwrap();
        assert_eq!(v["rate_limits"]["five_hour"]["used_percentage"], 26.0);
        assert_eq!(v["rate_limits"]["seven_day"]["used_percentage"], 27.0);
        let five = v["rate_limits"]["five_hour"]["resets_at"].as_i64().unwrap();
        assert_eq!(five, chrono::Local.with_ymd_and_hms(2026, 11, 10, 18, 19, 0).unwrap().timestamp());
        assert!(super::parse_usage_text("no limits here", now).is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unquote_roundtrip() {
        let original = r#""/opt/homebrew/bin/node" "/x/it's.js""#;
        let quoted = format!("'{}'", original.replace('\'', r"'\''"));
        assert_eq!(unquote(&quoted), original);
    }

    #[test]
    fn expired_window_is_zero() {
        let w = json!({"used_percentage": 80.0, "resets_at": 100.0});
        assert_eq!(window(Some(&w), 200.0, true)["used_percentage"], json!(0.0));
        assert_eq!(window(Some(&w), 50.0, true)["used_percentage"], json!(80.0));
    }

    #[test]
    fn missing_window_is_zero_only_once_limits_were_received() {
        assert_eq!(window(None, 0.0, true)["used_percentage"], json!(0.0));
        assert_eq!(window(None, 0.0, false), Value::Null);
    }
}
