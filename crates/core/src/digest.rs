//! Étage 1 de l'évaluation : le digest d'une session, calculé localement,
//! zéro token. Il attribue la lenteur à ses causes, dit si chacune est
//! optimisable, et chiffre le gain quand il est MESURABLE — sinon il le dit.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Approximation documentée : 1 token ≈ 4 octets de texte.
const BYTES_PER_TOKEN: i64 = 4;
/// Au-delà, une sortie d'outil est jugée volumineuse.
const BIG_RESULT: i64 = 20_000;
/// Taille qu'une sortie volumineuse aurait pu garder (head/grep/filtre).
const TRIMMED_RESULT: i64 = 8_000;
/// Un trou plus long n'est pas du temps modèle : c'est une pause.
const IDLE_CAP_MS: i64 = 30 * 60 * 1000;

struct Msg {
    agent: String,
    ts: i64,
    model: String,
    context: i64,
    output: i64,
    thinking: i64,
}

struct Call {
    id: String,
    agent: String,
    ts: i64,
    name: String,
    target: Option<String>,
    input_hash: i64,
    result_ts: Option<i64>,
    is_error: bool,
    result_bytes: i64,
    result_hash: Option<i64>,
    error_preview: Option<String>,
}

impl Call {
    fn duration(&self) -> i64 {
        self.result_ts.map_or(0, |r| (r - self.ts).max(0))
    }
}

struct Turn {
    agent: String,
    ts: i64,
    kind: String,
    preview: String,
}

#[derive(Serialize)]
pub struct Gain {
    pub ms: i64,
    pub tokens: i64,
    pub basis: String,
}

#[derive(Serialize)]
pub struct Cause {
    pub id: String,
    pub category: &'static str,
    pub title: String,
    pub ms: i64,
    pub tokens: i64,
    /// "oui" | "partiel" | "non"
    pub optimisable: &'static str,
    pub why: String,
    pub gain: Option<Gain>,
    pub items: Vec<Value>,
    /// Après « Optimiser » : date, et gaspillage apparu DEPUIS (distinct de celui d'avant).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixed: Option<Value>,
}

struct Data {
    msgs: Vec<Msg>,
    calls: Vec<Call>,
    turns: Vec<Turn>,
}

fn load(conn: &Connection, sid: &str) -> Result<Data> {
    let mut st = conn.prepare(
        "SELECT agent_id, ts, model, input + cache_write + cache_read, output, thinking
         FROM messages WHERE session_id = ?1 ORDER BY ts",
    )?;
    let msgs = st
        .query_map(params![sid], |r| {
            Ok(Msg { agent: r.get(0)?, ts: r.get(1)?, model: r.get(2)?, context: r.get(3)?, output: r.get(4)?, thinking: r.get(5)? })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut st = conn.prepare(
        "SELECT id, agent_id, ts, name, target, input_hash, result_ts, COALESCE(is_error, 0),
                COALESCE(result_bytes, 0), result_hash, error_preview
         FROM tool_calls WHERE session_id = ?1 ORDER BY ts",
    )?;
    let calls = st
        .query_map(params![sid], |r| {
            Ok(Call {
                id: r.get(0)?,
                agent: r.get(1)?,
                ts: r.get(2)?,
                name: r.get(3)?,
                target: r.get(4)?,
                input_hash: r.get(5)?,
                result_ts: r.get(6)?,
                is_error: r.get::<_, i64>(7)? != 0,
                result_bytes: r.get(8)?,
                result_hash: r.get(9)?,
                error_preview: r.get(10)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut st = conn.prepare("SELECT agent_id, ts, kind, preview FROM turns WHERE session_id = ?1 ORDER BY ts")?;
    let turns = st
        .query_map(params![sid], |r| Ok(Turn { agent: r.get(0)?, ts: r.get(1)?, kind: r.get(2)?, preview: r.get(3)? }))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Data { msgs, calls, turns })
}

/// Combien d'appels au modèle, du même agent, ont relu (via le cache) un
/// contenu entré à `ts` — jusqu'à la compaction suivante qui l'efface.
struct Carry {
    msg_ts: HashMap<String, Vec<i64>>,
    compact_ts: HashMap<String, Vec<i64>>,
}

impl Carry {
    fn new(d: &Data) -> Self {
        let mut msg_ts: HashMap<String, Vec<i64>> = HashMap::new();
        for m in &d.msgs {
            msg_ts.entry(m.agent.clone()).or_default().push(m.ts);
        }
        let mut compact_ts: HashMap<String, Vec<i64>> = HashMap::new();
        for t in d.turns.iter().filter(|t| t.kind == "compact") {
            compact_ts.entry(t.agent.clone()).or_default().push(t.ts);
        }
        Carry { msg_ts, compact_ts }
    }

    fn calls_after(&self, agent: &str, ts: i64) -> i64 {
        let Some(v) = self.msg_ts.get(agent) else { return 0 };
        let end = self
            .compact_ts
            .get(agent)
            .and_then(|c| c.iter().copied().find(|&c| c > ts))
            .unwrap_or(i64::MAX);
        let a = v.partition_point(|&t| t <= ts);
        let b = v.partition_point(|&t| t < end);
        (b.saturating_sub(a)) as i64
    }

    /// Tokens relus au total pour `bytes` octets entrés à `ts`, entrée comprise.
    fn tokens(&self, agent: &str, ts: i64, bytes: i64) -> i64 {
        (bytes / BYTES_PER_TOKEN) * (1 + self.calls_after(agent, ts))
    }
}

/// Union d'intervalles : le temps d'outil réel quand des appels se chevauchent.
fn union_ms(mut iv: Vec<(i64, i64)>) -> i64 {
    iv.sort_unstable();
    let (mut total, mut cur): (i64, Option<(i64, i64)>) = (0, None);
    for (a, b) in iv {
        cur = match cur {
            Some((s, e)) if a <= e => Some((s, e.max(b))),
            Some((s, e)) => {
                total += e - s;
                Some((a, b))
            }
            None => Some((a, b)),
        };
    }
    total + cur.map_or(0, |(s, e)| e - s)
}

/// Tête lisible d'une commande shell : on saute les `cd …` de préambule.
pub fn command_head(cmd: &str) -> String {
    let seg = cmd
        .split("&&")
        .flat_map(|s| s.split(';'))
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with("cd ") && !s.starts_with("export "))
        .map(|s| s.split_whitespace().skip_while(|w| is_assignment(w)).collect::<Vec<_>>())
        .find(|words| !words.is_empty());
    match seg {
        Some(words) => words.into_iter().take(2).collect::<Vec<_>>().join(" "),
        None => cmd.split_whitespace().take(2).collect::<Vec<_>>().join(" "),
    }
}

/// `NOM=valeur` en tête de commande (affectation shell).
fn is_assignment(w: &str) -> bool {
    match w.split_once('=') {
        Some((k, _)) => !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
        None => false,
    }
}

const POLL_PREFIX: &str = "poll:";

fn tool_bucket(c: &Call) -> String {
    match (c.name.as_str(), &c.target) {
        ("Bash", Some(t)) => {
            let head = command_head(t);
            let first = head.split_whitespace().next().unwrap_or("");
            if matches!(first, "until" | "while" | "sleep") {
                format!("{POLL_PREFIX}{head}")
            } else {
                format!("Bash · {head}")
            }
        }
        (n, _) => n.to_string(),
    }
}

const CORRECTION_MARKERS: [&str; 16] = [
    "non", "no,", "no ", "pas ça", "arrête", "stop", "je t'ai dit", "j'ai dit", "tu as oublié", "ce n'est pas",
    "c'est faux", "wrong", "that's not", "revert", "annule", "pourquoi tu",
];

fn looks_like_correction(p: &str) -> bool {
    let l = p.trim_start().to_lowercase();
    CORRECTION_MARKERS.iter().any(|m| l.starts_with(m))
}

fn median(mut v: Vec<i64>) -> i64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[v.len() / 2]
}

pub fn digest(conn: &Connection, sid: &str) -> Result<Value> {
    let d = load(conn, sid)?;
    let carry = Carry::new(&d);
    let session: Option<(Option<f64>, Option<f64>, Option<f64>, Option<f64>)> = conn
        .query_row("SELECT cost_usd, api_ms, tool_ms, total_ms FROM sessions WHERE id = ?1", params![sid], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .ok();
    let (cost_usd, api_ms, tool_ms_official, _total_ms) = session.unwrap_or((None, None, None, None));

    // ------------------------------------------------ chronologie de l'agent principal
    // Points d'événement : chaque message, chaque résultat d'outil, chaque tour humain.
    let main_msgs: Vec<&Msg> = d.msgs.iter().filter(|m| m.agent.is_empty()).collect();
    let mut points: Vec<(i64, u8)> = Vec::new(); // 0 = message, 1 = résultat, 2 = humain
    points.extend(main_msgs.iter().map(|m| (m.ts, 0)));
    points.extend(d.calls.iter().filter(|c| c.agent.is_empty()).filter_map(|c| c.result_ts.map(|t| (t, 1))));
    points.extend(d.turns.iter().filter(|t| t.agent.is_empty() && matches!(t.kind.as_str(), "prompt" | "command")).map(|t| (t.ts, 2)));
    points.sort_unstable();

    let (mut human_ms, mut model_ms_est, mut pause_ms) = (0i64, 0i64, 0i64);
    for w in points.windows(2) {
        let gap = (w[1].0 - w[0].0).max(0);
        match w[1].1 {
            _ if gap > IDLE_CAP_MS => pause_ms += gap,
            2 => human_ms += gap,
            0 => model_ms_est += gap,
            _ => {}
        }
    }
    let tool_wall = union_ms(
        d.calls.iter().filter(|c| c.agent.is_empty()).filter_map(|c| c.result_ts.map(|r| (c.ts, r.max(c.ts)))).collect(),
    );
    let model_ms = api_ms.map(|x| x as i64).unwrap_or(model_ms_est);
    let model_official = api_ms.is_some();

    // ------------------------------------------------ causes
    let mut causes: Vec<Cause> = Vec::new();
    // Pour chaque cause optimisable : d'où vient chaque gaspillage (moment, ms, tokens).
    let mut origins: HashMap<String, Vec<(i64, i64, i64)>> = HashMap::new();

    // Temps modèle.
    let thinking: i64 = main_msgs.iter().map(|m| m.thinking).sum();
    let output: i64 = main_msgs.iter().map(|m| m.output).sum();
    let mut slow_calls: Vec<(i64, &Msg)> = Vec::new();
    {
        let by_ts: HashMap<i64, &Msg> = main_msgs.iter().map(|m| (m.ts, *m)).collect();
        let mut last = i64::MIN;
        for &(t, kind) in &points {
            if kind == 0 && last != i64::MIN && t - last <= IDLE_CAP_MS {
                if let Some(m) = by_ts.get(&t).copied() {
                    slow_calls.push((t - last, m));
                }
            }
            last = t;
        }
        slow_calls.sort_by(|a, b| b.0.cmp(&a.0));
    }
    causes.push(Cause {
        id: "model".into(),
        category: "modèle",
        title: "Modèle".into(),
        ms: model_ms,
        tokens: output,
        optimisable: "partiel",
        fixed: None,
        why: format!("Dont {thinking} tokens de réflexion sur {output}. Gain non chiffrable."),
        gain: None,
        items: slow_calls
            .iter()
            .take(8)
            .map(|(ms, m)| json!({"ts": m.ts, "ms": ms, "model": m.model, "thinking": m.thinking, "output": m.output, "context": m.context}))
            .collect(),
    });

    // Attente humaine.
    causes.push(Cause {
        id: "human".into(),
        category: "humain",
        title: "Ton attente".into(),
        ms: human_ms,
        tokens: 0,
        optimisable: "non",
        fixed: None,
        why: "Hors agent.".into(),
        gain: None,
        items: vec![],
    });
    if pause_ms > 0 {
        causes.push(Cause {
            id: "pause".into(),
            category: "humain",
            title: "Pauses".into(),
            ms: pause_ms,
            tokens: 0,
            optimisable: "non",
            fixed: None,
            why: "Session ouverte ou reprise plus tard.".into(),
            gain: None,
            items: vec![],
        });
    }

    // Outils, par famille.
    let mut buckets: HashMap<String, (i64, i64, i64)> = HashMap::new(); // ms, appels, erreurs
    for c in d.calls.iter().filter(|c| c.agent.is_empty()) {
        let e = buckets.entry(tool_bucket(c)).or_default();
        e.0 += c.duration();
        e.1 += 1;
        e.2 += c.is_error as i64;
    }
    let mut bv: Vec<_> = buckets.into_iter().collect();
    bv.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    for (name, (ms, n, errs)) in bv.iter().take(6) {
        if *ms < 1000 {
            continue;
        }
        let mut slowest: Vec<&Call> = d.calls.iter().filter(|c| c.agent.is_empty() && &tool_bucket(c) == name).collect();
        slowest.sort_by(|a, b| b.duration().cmp(&a.duration()));
        if let Some(poll) = name.strip_prefix(POLL_PREFIX) {
            causes.push(Cause {
                id: format!("tool:{name}"),
                category: "attente",
                title: format!("Attente active · {poll} ({n} appels)"),
                ms: *ms,
                tokens: 0,
                optimisable: "partiel",
                fixed: None,
                why: "L'agent attend une tâche de fond au lieu d'avancer.".into(),
                gain: None,
                items: slowest.iter().take(8).map(|c| json!({"ts": c.ts, "ms": c.duration(), "target": c.target, "error": c.is_error})).collect(),
            });
            continue;
        }
        causes.push(Cause {
            id: format!("tool:{name}"),
            category: "outils",
            title: format!("{name} ({n} appels)"),
            ms: *ms,
            tokens: 0,
            optimisable: "non",
            fixed: None,
            why: format!(
                "Durée de l'outil lui-même{}. Levier hors IA.",
                if *errs > 0 { format!(", {errs} échecs") } else { String::new() }
            ),
            gain: None,
            items: slowest.iter().take(8).map(|c| json!({"ts": c.ts, "ms": c.duration(), "target": c.target, "error": c.is_error})).collect(),
        });
    }

    // Appels en échec.
    // Seuls les échecs ÉVITABLES comptent : une commande qui échoue avec sa propre
    // sortie (test rouge, compilateur qui refuse) est du travail normal.
    let failed: Vec<&Call> = d
        .calls
        .iter()
        .filter(|c| c.is_error && c.error_preview.as_deref().and_then(crate::fixes::avoidable).is_some())
        .collect();
    if !failed.is_empty() {
        let ms: i64 = failed.iter().map(|c| c.duration()).sum();
        let tokens: i64 = failed.iter().map(|c| carry.tokens(&c.agent, c.result_ts.unwrap_or(c.ts), c.result_bytes)).sum();
        origins.insert(
            "failed".into(),
            failed.iter().map(|c| (c.ts, c.duration(), carry.tokens(&c.agent, c.result_ts.unwrap_or(c.ts), c.result_bytes))).collect(),
        );
        causes.push(Cause {
            id: "failed".into(),
            category: "gaspillage",
            title: format!("Échecs évitables ({})", failed.len()),
            ms,
            tokens,
            optimisable: "oui",
            fixed: None,
            why: "Chemin inexistant, script qui plante, commande bloquée ou tuée : durée perdue et erreur relue à chaque tour.".into(),
            gain: Some(Gain { ms, tokens, basis: "durée des échecs + erreur relue jusqu'à compaction".into() }),
            items: failed
                .iter()
                .take(30)
                .map(|c| json!({"ts": c.ts, "ms": c.duration(), "name": c.name, "target": c.target, "error": c.error_preview}))
                .collect(),
        });
    }

    // Répétitions à l'identique : même entrée ET même résultat.
    let mut groups: HashMap<(String, i64, i64), Vec<&Call>> = HashMap::new();
    for c in &d.calls {
        if let Some(rh) = c.result_hash {
            groups.entry((c.name.clone(), c.input_hash, rh)).or_default().push(c);
        }
    }
    let mut repeats: Vec<(&Call, usize, i64, i64)> = Vec::new(); // premier, n, ms perdus, tokens perdus
    let mut repeat_origins = Vec::new();
    for g in groups.values().filter(|g| g.len() >= 2) {
        let wasted = &g[1..];
        for c in wasted {
            repeat_origins.push((c.ts, c.duration(), carry.tokens(&c.agent, c.result_ts.unwrap_or(c.ts), c.result_bytes)));
        }
        repeats.push((
            g[0],
            g.len(),
            wasted.iter().map(|c| c.duration()).sum(),
            wasted.iter().map(|c| carry.tokens(&c.agent, c.result_ts.unwrap_or(c.ts), c.result_bytes)).sum(),
        ));
    }
    origins.insert("repeats".into(), repeat_origins);
    if !repeats.is_empty() {
        repeats.sort_by(|a, b| (b.2 + b.3).cmp(&(a.2 + a.3)));
        let ms: i64 = repeats.iter().map(|r| r.2).sum();
        let tokens: i64 = repeats.iter().map(|r| r.3).sum();
        causes.push(Cause {
            id: "repeats".into(),
            category: "gaspillage",
            title: format!("Répétitions ({})", repeats.len()),
            ms,
            tokens,
            optimisable: "oui",
            fixed: None,
            why: "Même entrée, même résultat : c'était déjà dans le contexte.".into(),
            gain: Some(Gain { ms, tokens, basis: "répétitions après la première + leur sortie relue".into() }),
            items: repeats
                .iter()
                .take(30)
                .map(|(c, n, ms, tk)| json!({"name": c.name, "target": c.target, "count": n, "ms": ms, "tokens": tk}))
                .collect(),
        });
    }

    // Sorties volumineuses portées dans le contexte.
    let mut big: Vec<(&Call, i64)> = d
        .calls
        .iter()
        .filter(|c| c.result_bytes > BIG_RESULT)
        .map(|c| {
            let excess = c.result_bytes - TRIMMED_RESULT;
            let after = carry.calls_after(&c.agent, c.result_ts.unwrap_or(c.ts));
            (c, (excess / BYTES_PER_TOKEN) * (1 + after))
        })
        .collect();
    origins.insert("big-results".into(), big.iter().map(|(c, tk)| (c.ts, 0, *tk)).collect());
    if !big.is_empty() {
        big.sort_by(|a, b| b.1.cmp(&a.1));
        let tokens: i64 = big.iter().map(|b| b.1).sum();
        causes.push(Cause {
            id: "big-results".into(),
            category: "contexte",
            title: format!("Grosses sorties ({})", big.len()),
            ms: 0,
            tokens,
            optimisable: "oui",
            fixed: None,
            why: "Relue à chaque appel suivant. À filtrer (head, grep).".into(),
            gain: Some(Gain {
                ms: 0,
                tokens,
                basis: format!("sorties ramenées à {} Ko × relectures", TRIMMED_RESULT / 1000),
            }),
            items: big
                .iter()
                .take(30)
                .map(|(c, tk)| json!({"ts": c.ts, "name": c.name, "target": c.target, "bytes": c.result_bytes, "tokens": tk}))
                .collect(),
        });
    }

    // Erreurs d'API.
    let api_errors: Vec<&Turn> = d.turns.iter().filter(|t| t.kind == "api_error").collect();
    if !api_errors.is_empty() {
        causes.push(Cause {
            id: "api-errors".into(),
            category: "service",
            title: format!("Erreurs API ({})", api_errors.len()),
            ms: 0,
            tokens: 0,
            optimisable: "non",
            fixed: None,
            why: "Côté service.".into(),
            gain: None,
            items: api_errors.iter().take(20).map(|t| json!({"ts": t.ts, "text": t.preview})).collect(),
        });
    }

    // Optimisations déjà appliquées dans ce projet : on sépare le gaspillage d'avant et celui d'après.
    let first_ts: Option<i64> = conn.query_row("SELECT first_ts FROM sessions WHERE id = ?1", params![sid], |r| r.get(0)).ok().flatten();
    let mut fixes: HashMap<String, i64> = HashMap::new();
    if let Ok(mut st) = conn.prepare(
        "SELECT f.cause, MAX(f.ts) FROM fixes f
         WHERE f.repo_id = (SELECT m.repo_id FROM sessions s JOIN cwd_map m ON m.cwd = s.cwd WHERE s.id = ?1)
         GROUP BY f.cause",
    ) {
        if let Ok(rows) = st.query_map(params![sid], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))) {
            fixes.extend(rows.flatten());
        }
    }
    for c in causes.iter_mut() {
        let at = if c.id.starts_with("tool:poll:") {
            fixes.iter().filter(|(k, _)| k.starts_with("tool:poll:")).map(|(_, t)| *t).max()
        } else {
            fixes.get(&c.id).copied()
        };
        let Some(at) = at else { continue };
        let (mut new_tokens, mut new_ms) = (0i64, 0i64);
        if c.id.starts_with("tool:poll:") {
            // Attente active : chaque appel de la famille compte pour sa durée.
            let fam = c.id.trim_start_matches("tool:");
            for call in d.calls.iter().filter(|x| x.agent.is_empty() && x.ts > at && tool_bucket(x) == fam) {
                new_ms += call.duration();
            }
        } else if let Some(o) = origins.get(&c.id) {
            for (ts, ms, tk) in o.iter().filter(|(ts, _, _)| *ts > at) {
                let _ = ts;
                new_ms += ms;
                new_tokens += tk;
            }
        }
        c.fixed = Some(json!({
            "at": at,
            // Claude Code lit le CLAUDE.md au démarrage : une session déjà ouverte ne voit pas la règle.
            "session_started_before": first_ts.is_some_and(|f| f < at),
            "new_tokens": new_tokens,
            "new_ms": new_ms,
        }));
    }

    // Ce qui est optimisable d'abord, puis par temps, puis par tokens.
    let rank = |o: &str| match o {
        "oui" => 0,
        "partiel" => 1,
        _ => 2,
    };
    causes.sort_by(|a, b| {
        rank(a.optimisable).cmp(&rank(b.optimisable)).then(b.ms.cmp(&a.ms)).then(b.tokens.cmp(&a.tokens))
    });

    // ------------------------------------------------ signaux qualité
    let prompts: Vec<&Turn> = d.turns.iter().filter(|t| t.agent.is_empty() && t.kind == "prompt").collect();
    let corrections: Vec<&Turn> = prompts.iter().copied().filter(|t| looks_like_correction(&t.preview)).collect();
    let count = |k: &str| d.turns.iter().filter(|t| t.kind == k).count();

    let total_input: i64 = d.msgs.iter().map(|m| m.context).sum();
    let cache_read: i64 = conn.query_row("SELECT COALESCE(SUM(cache_read), 0) FROM messages WHERE session_id = ?1", params![sid], |r| r.get(0))?;
    let contexts: Vec<i64> = main_msgs.iter().map(|m| m.context).collect();

    let mut models: Vec<String> = d.msgs.iter().map(|m| m.model.clone()).collect();
    models.sort();
    models.dedup();
    let mut agents: Vec<&str> = d.msgs.iter().map(|m| m.agent.as_str()).filter(|a| !a.is_empty()).collect();
    agents.sort();
    agents.dedup();

    let gain_ms: i64 = causes.iter().filter_map(|c| c.gain.as_ref()).map(|g| g.ms).sum();
    let gain_tokens: i64 = causes.iter().filter_map(|c| c.gain.as_ref()).map(|g| g.tokens).sum();

    Ok(json!({
        "session_id": sid,
        "cost_usd": cost_usd,
        "time": {
            "model_ms": model_ms,
            "model_official": model_official,
            "tool_ms": tool_ms_official.map(|x| x as i64).unwrap_or(tool_wall),
            "tool_official": tool_ms_official.is_some(),
            "human_ms": human_ms,
            "pause_ms": pause_ms,
        },
        "tokens": {
            "context_total": total_input,
            "cache_read": cache_read,
            "cache_ratio": if total_input > 0 { cache_read as f64 / total_input as f64 } else { 0.0 },
            "output": d.msgs.iter().map(|m| m.output).sum::<i64>(),
            "thinking": d.msgs.iter().map(|m| m.thinking).sum::<i64>(),
            "first_context": contexts.first().copied().unwrap_or(0),
            "median_context": median(contexts.clone()),
            "max_context": contexts.iter().copied().max().unwrap_or(0),
        },
        "counts": {
            "api_calls": d.msgs.len(),
            "tool_calls": d.calls.len(),
            "tool_errors": failed.len(),
            "prompts": prompts.len(),
            "interrupts": count("interrupt"),
            "compactions": count("compact"),
            "api_errors": api_errors.len(),
            "subagents": agents.len(),
        },
        "models": models,
        "fixable": causes.iter().filter(|c| crate::fixes::is_fixable(&c.id)).map(|c| c.id.clone()).collect::<Vec<_>>(),
        "causes": causes,
        "gain_total": { "ms": gain_ms, "tokens": gain_tokens },
        "corrections": corrections.iter().map(|t| json!({"ts": t.ts, "text": t.preview})).collect::<Vec<_>>(),
        "interrupts": d.turns.iter().filter(|t| t.kind == "interrupt").map(|t| json!({"ts": t.ts})).collect::<Vec<_>>(),
    }))
}

/// Chronologie compacte d'une session (agent principal + sous-agents).
pub fn timeline(conn: &Connection, sid: &str, limit: i64) -> Result<Value> {
    let d = load(conn, sid)?;
    let mut ev: Vec<(i64, Value)> = Vec::new();
    for c in &d.calls {
        ev.push((
            c.ts,
            json!({"t": "tool", "ts": c.ts, "agent": c.agent, "name": c.name, "target": c.target, "ms": c.duration(),
                   "error": c.is_error, "bytes": c.result_bytes, "id": c.id}),
        ));
    }
    for t in &d.turns {
        ev.push((t.ts, json!({"t": t.kind, "ts": t.ts, "agent": t.agent, "text": t.preview})));
    }
    ev.sort_by_key(|e| e.0);
    let n = ev.len();
    let skip = n.saturating_sub(limit.max(0) as usize);
    Ok(json!({"total": n, "events": ev.into_iter().skip(skip).map(|e| e.1).collect::<Vec<_>>()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_overlaps() {
        assert_eq!(union_ms(vec![(0, 10), (5, 15), (20, 25)]), 20);
        assert_eq!(union_ms(vec![]), 0);
    }

    #[test]
    fn heads() {
        assert_eq!(command_head("cd tests && make unit-fast"), "make unit-fast");
        assert_eq!(command_head("git status"), "git status");
        assert_eq!(command_head("S=/tmp/x; cd $S && CC=clang make -j4"), "make -j4");
        assert_eq!(command_head("A=1 B=2"), "A=1 B=2");
    }

    #[test]
    fn corrections() {
        assert!(looks_like_correction("Non, je t'ai dit de ne pas toucher"));
        assert!(!looks_like_correction("Nouvelle tâche"));
    }
}
