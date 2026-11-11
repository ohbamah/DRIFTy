//! Automations (page « Automations ») : des flows de nœuds reliés, à la manière de n8n. Un
//! flow part de son nœud Start (fixe) ; les autres nœuds (GitHub, IA, commande bash, webhook)
//! se posent et se relient dans l'éditeur. Ce module les GARDE et vérifie leur forme ;
//! l'exécution viendra quand les nœuds seront définis.

use anyhow::Result;
use axum::extract::{Path, State};
use axum::Json;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

use crate::{with_conn, ApiResult, App};
use drifty_core::db;

const KEY: &str = "automations";
/// Types de nœuds connus (le Start est unique et fixe).
pub const KINDS: [&str; 5] = ["start", "github", "ai", "bash", "webhook"];
const MAX_NODES: usize = 200;
const MAX_BYTES: usize = 512 * 1024;

fn all(c: &Connection) -> Result<Vec<Value>> {
    Ok(db::setting(c, KEY)?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default())
}

fn save_all(c: &Connection, flows: &[Value]) -> Result<()> {
    db::set_setting(c, KEY, &serde_json::to_string(flows)?)
}

/// Vérifie la forme d'un flow et le rend propre (champs connus seulement).
pub fn check(id: &str, f: &Value) -> Result<Value> {
    if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        anyhow::bail!("The automation `{id}` cannot be saved because its id must be 1 to 64 letters, digits, `-` or `_`.");
    }
    if f.to_string().len() > MAX_BYTES {
        anyhow::bail!("The automation `{id}` cannot be saved because it is larger than 512 KB.");
    }
    let name = f["name"].as_str().unwrap_or("").trim();
    if name.is_empty() || name.chars().count() > 80 {
        anyhow::bail!("The automation `{id}` cannot be saved because its name must be 1 to 80 characters long.");
    }
    let nodes = f["nodes"].as_array().cloned().unwrap_or_default();
    if nodes.len() > MAX_NODES {
        anyhow::bail!("The automation `{name}` cannot be saved because it has more than {MAX_NODES} nodes.");
    }
    let mut ids = HashSet::new();
    let mut kinds = HashMap::new();
    let mut clean_nodes = Vec::new();
    for n in &nodes {
        let nid = n["id"].as_str().unwrap_or("");
        let kind = n["type"].as_str().unwrap_or("");
        if nid.is_empty() || !ids.insert(nid.to_string()) {
            anyhow::bail!("The automation `{name}` cannot be saved because a node has an empty or repeated id `{nid}`.");
        }
        if !KINDS.contains(&kind) {
            anyhow::bail!("The node `{nid}` of `{name}` cannot be saved because its type `{kind}` is unknown.");
        }
        kinds.insert(nid.to_string(), kind.to_string());
        let config = if n["config"].is_object() { n["config"].clone() } else { json!({}) };
        clean_nodes.push(json!({"id": nid, "type": kind, "x": n["x"].as_f64().unwrap_or(0.0), "y": n["y"].as_f64().unwrap_or(0.0), "config": config}));
    }
    let starts: Vec<&String> = kinds.iter().filter(|(_, k)| *k == "start").map(|(id, _)| id).collect();
    if starts.len() != 1 {
        anyhow::bail!("The automation `{name}` cannot be saved because it has {} Start nodes instead of one.", starts.len());
    }
    let mut seen = HashSet::new();
    let mut next: HashMap<String, Vec<String>> = HashMap::new();
    let mut clean_edges = Vec::new();
    for e in f["edges"].as_array().cloned().unwrap_or_default() {
        let (a, b) = (e["from"].as_str().unwrap_or(""), e["to"].as_str().unwrap_or(""));
        if !kinds.contains_key(a) || !kinds.contains_key(b) {
            anyhow::bail!("The link `{a}` → `{b}` of `{name}` cannot be saved because one of its nodes does not exist.");
        }
        if a == b || kinds[b] == "start" {
            anyhow::bail!("The link `{a}` → `{b}` of `{name}` cannot be saved because nothing can lead into the Start node or into itself.");
        }
        if seen.insert((a.to_string(), b.to_string())) {
            next.entry(a.to_string()).or_default().push(b.to_string());
            clean_edges.push(json!({"from": a, "to": b}));
        }
    }
    if let Some(n) = cycle(&next) {
        anyhow::bail!("The automation `{name}` cannot be saved because its links make a loop through `{n}`.");
    }
    Ok(json!({"id": id, "name": name, "nodes": clean_nodes, "edges": clean_edges, "updated": f["updated"].as_i64().unwrap_or(0)}))
}

/// Un nœud par lequel passe une boucle, s'il y en a une.
fn cycle(next: &HashMap<String, Vec<String>>) -> Option<String> {
    fn visit(n: &str, next: &HashMap<String, Vec<String>>, state: &mut HashMap<String, u8>) -> Option<String> {
        match state.get(n) {
            Some(1) => return Some(n.to_string()),
            Some(2) => return None,
            _ => {}
        }
        state.insert(n.to_string(), 1);
        for m in next.get(n).into_iter().flatten() {
            if let Some(c) = visit(m, next, state) {
                return Some(c);
            }
        }
        state.insert(n.to_string(), 2);
        None
    }
    let mut state = HashMap::new();
    next.keys().find_map(|n| visit(n, next, &mut state))
}

pub async fn route_list(State(app): State<App>) -> ApiResult {
    with_conn(&app, |c| Ok(json!(all(c)?))).await
}

pub async fn route_put(State(app): State<App>, Path(id): Path<String>, Json(f): Json<Value>) -> ApiResult {
    with_conn(&app, move |c| {
        let mut flow = check(&id, &f)?;
        flow["updated"] = json!(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64));
        let mut flows = all(c)?;
        match flows.iter_mut().find(|x| x["id"] == id.as_str()) {
            Some(x) => *x = flow.clone(),
            None => flows.push(flow.clone()),
        }
        save_all(c, &flows)?;
        Ok(flow)
    })
    .await
}

pub async fn route_delete(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    with_conn(&app, move |c| {
        let mut flows = all(c)?;
        flows.retain(|x| x["id"] != id.as_str());
        save_all(c, &flows)?;
        Ok(json!({"ok": true}))
    })
    .await
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    fn flow(nodes: serde_json::Value, edges: serde_json::Value) -> serde_json::Value {
        json!({"name": "Revue", "nodes": nodes, "edges": edges})
    }
    fn n(id: &str, t: &str) -> serde_json::Value {
        json!({"id": id, "type": t, "x": 1, "y": 2, "config": {"k": "v"}, "extra": "dropped"})
    }

    #[test]
    fn a_valid_flow_is_kept_and_cleaned() {
        let f = super::check("f1", &flow(json!([n("start", "start"), n("a", "ai"), n("b", "bash")]), json!([{"from": "start", "to": "a"}, {"from": "a", "to": "b"}, {"from": "a", "to": "b"}]))).unwrap();
        assert_eq!(f["edges"].as_array().unwrap().len(), 2); // doublon retiré
        assert!(f["nodes"][1].get("extra").is_none());
        assert_eq!(f["nodes"][1]["config"]["k"], "v");
    }

    #[test]
    fn malformed_flows_are_refused_by_name() {
        let refuse = |f: serde_json::Value| super::check("f1", &f).unwrap_err().to_string();
        assert!(refuse(flow(json!([n("a", "ai")]), json!([]))).contains("0 Start nodes"));
        assert!(refuse(flow(json!([n("start", "start"), n("s2", "start")]), json!([]))).contains("2 Start nodes"));
        assert!(refuse(flow(json!([n("start", "start"), n("x", "slack")]), json!([]))).contains("type `slack` is unknown"));
        assert!(refuse(flow(json!([n("start", "start"), n("a", "ai")]), json!([{"from": "a", "to": "start"}]))).contains("into the Start node"));
        assert!(refuse(flow(json!([n("start", "start"), n("a", "ai")]), json!([{"from": "a", "to": "zz"}]))).contains("does not exist"));
        assert!(refuse(flow(json!([n("start", "start"), n("a", "ai"), n("b", "bash")]), json!([{"from": "a", "to": "b"}, {"from": "b", "to": "a"}]))).contains("loop"));
        assert!(super::check("../x", &flow(json!([n("start", "start")]), json!([]))).is_err());
    }
}
