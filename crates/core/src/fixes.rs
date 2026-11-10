//! Optimiser : transforme une cause du digest en règles pour le `CLAUDE.md` du
//! projet. Rien n'est écrit sans demande explicite (`apply`).
//!
//! Le bloc DRIFTy ne peut pas grossir sans fin : il ne contient que des
//! règles d'un CATALOGUE fini, au plus une ligne par type, repérée par une clé
//! stable. Réappliquer met une règle à jour (ses exemples fusionnent, plafonnés)
//! au lieu de l'ajouter une seconde fois. Taille maximale = taille du catalogue.

use crate::digest;
use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const START: &str = "<!-- claudius:start -->";
const END: &str = "<!-- claudius:end -->";
const HEADING: &str = "## Règles ajoutées par DRIFTy";
/// Exemples gardés au plus par règle (les plus récents d'abord).
const MAX_ITEMS: usize = 5;

/// Un type de règle. `list` : la règle porte des exemples (`a`, `b`…).
struct Kind {
    key: &'static str,
    list: bool,
    render: fn(&str) -> String,
}

fn ticks(items: &str) -> String {
    items.to_string()
}

/// Le catalogue, dans l'ordre d'affichage du bloc. C'est lui qui borne la taille.
const CATALOGUE: [Kind; 14] = [
    Kind { key: "path", list: false, render: |_| "Vérifie qu'un chemin existe (ls, Glob) avant de l'utiliser : le dossier courant n'est pas toujours celui que tu crois.".into() },
    Kind { key: "missing-cmd", list: true, render: |l| format!("Ces commandes n'existent pas dans cet environnement, ne les utilise pas : {}.", ticks(l)) },
    Kind { key: "code-shell", list: false, render: |_| "Du code source a été exécuté comme commande shell : écris-le dans un fichier (heredoc) au lieu de le passer en ligne de commande.".into() },
    Kind { key: "python", list: false, render: |_| "Un script Python en ligne a planté : teste-le sur un petit cas avant de l'appliquer en grand.".into() },
    Kind { key: "killed", list: false, render: |_| "Une commande a été tuée (mémoire ou délai) : réduis son périmètre ou lance-la en arrière-plan.".into() },
    Kind { key: "read-first", list: false, render: |_| "Lis un fichier avant de l'éditer ou de l'écraser.".into() },
    Kind { key: "edit-exact", list: false, render: |_| "Avant un Edit, relis la zone exacte : le texte remplacé doit correspondre au caractère près.".into() },
    Kind { key: "worktree", list: false, render: |_| "Un sous-agent isolé dans un worktree travaille dans SON worktree, jamais dans le dépôt principal.".into() },
    Kind { key: "safety", list: false, render: |_| "Évite les commandes que le contrôle de sécurité bloque (suppression via une variable non vérifiée, commande difficile à analyser) : écris un script dans un fichier.".into() },
    Kind { key: "sleep", list: false, render: |_| "N'enchaîne pas `sleep` puis une lecture : lance la tâche en arrière-plan et attends sa notification.".into() },
    Kind { key: "poll", list: false, render: |_| "N'attends pas une tâche de fond avec une boucle `until`/`sleep` : lance-la en arrière-plan et avance sur autre chose jusqu'à sa notification.".into() },
    Kind { key: "repeat", list: true, render: |l| {
        if l.is_empty() {
            "Ne relis pas un fichier et ne relance pas une commande dont le résultat est déjà dans le contexte, sauf si quelque chose a changé.".into()
        } else {
            format!("Ne relis pas un fichier et ne relance pas une commande dont le résultat est déjà dans le contexte, sauf si quelque chose a changé (déjà répétés pour rien : {l}).")
        }
    } },
    Kind { key: "big-cmd", list: true, render: |l| format!("Ces commandes produisent des sorties énormes : filtre-les (tail, grep, options silencieuses) au lieu de les lire en entier : {l}.") },
    Kind { key: "big-read", list: false, render: |_| "Lis un gros fichier par morceaux (offset/limit) ou cherche dedans avec Grep, au lieu de le lire en entier.".into() },
];

fn kind(key: &str) -> Option<&'static Kind> {
    CATALOGUE.iter().find(|k| k.key == key)
}

fn render(key: &str, items: &[String]) -> String {
    let list = items.iter().map(|i| format!("`{i}`")).collect::<Vec<_>>().join(", ");
    kind(key).map(|k| (k.render)(&list)).unwrap_or_default()
}

/// Le texte d'une règle du catalogue (avec son exemple éventuel), pour la proposer en direct.
pub fn rule_text(key: &str, item: Option<&str>) -> String {
    render(key, &item.map(|i| vec![i.to_string()]).unwrap_or_default())
}

/// Une règle trouvée : sa clé, et l'exemple qu'elle apporte éventuellement.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub key: &'static str,
    pub item: Option<String>,
}

/// Un échec d'outil est-il ÉVITABLE ? Une commande qui sort en erreur avec sa
/// propre sortie (un test qui échoue, un compilateur qui refuse) est un
/// résultat de travail normal : `None`.
pub fn avoidable(error: &str) -> Option<Found> {
    let e = error.to_lowercase();
    if e.contains("doesn't want to") || e.contains("user rejected") || e.contains("denied by the user") {
        return None; // refus de l'utilisateur : pas un gaspillage de l'agent
    }
    if let Some(name) = missing_command(&e) {
        // Un mot-clé de langage « introuvable » : du code a été exécuté comme commande shell.
        const CODE: [&str; 22] = [
            "static", "int", "void", "return", "if", "else", "for", "while", "class", "struct", "const", "def", "import",
            "function", "fn", "let", "var", "public", "private", "#include", "template", "namespace",
        ];
        if CODE.contains(&name.as_str()) || name.ends_with(';') || name.contains('(') {
            return Some(Found { key: "code-shell", item: None });
        }
        return Some(Found { key: "missing-cmd", item: Some(name) });
    }
    let rules: [(&[&str], &'static str); 8] = [
        (&["isolated in the worktree"], "worktree"),
        (&["blocked: sleep"], "sleep"),
        (&["safety check"], "safety"),
        (&["file has not been read yet"], "read-first"),
        (&["string to replace not found", "old_string"], "edit-exact"),
        (&["traceback (most recent call last)"], "python"),
        (&["exit code 137", "killed", "timed out", "timeout"], "killed"),
        (&["no such file or directory", "can't open file", "does not exist", "cannot find the path"], "path"),
    ];
    rules.iter().find(|(pats, _)| pats.iter().any(|p| e.contains(p))).map(|(_, k)| Found { key: k, item: None })
}

/// Nom de la commande introuvable. zsh : `command not found: rg` ; bash : `rg: command not found`.
fn missing_command(e: &str) -> Option<String> {
    let word = |s: &str| s.trim().split(|c: char| c.is_whitespace() || c == ':').find(|w| !w.is_empty()).map(str::to_string);
    let name = if let Some((_, after)) = e.split_once("command not found: ") {
        word(after)
    } else if let Some((before, _)) = e.split_once(": command not found") {
        before.rsplit(|c: char| c.is_whitespace() || c == ':').find(|w| !w.is_empty()).map(str::to_string)
    } else {
        None
    }?;
    // Un nom de commande contient au moins une lettre (pas un numéro de ligne « (eval):4 »).
    name.chars().any(|c| c.is_alphabetic()).then_some(name)
}

/// Un exemple qui n'a de sens que dans cette session (fichier temporaire) n'entre pas dans une règle durable.
fn durable(target: &str) -> bool {
    !(target.contains("/tmp/") || target.contains("/private/") || target.contains("scratchpad") || target.starts_with("select:"))
}

/// Les causes pour lesquelles une règle concrète existe.
pub fn is_fixable(cause_id: &str) -> bool {
    matches!(cause_id, "failed" | "repeats" | "big-results") || cause_id.starts_with("tool:poll:")
}

fn short_path(p: &str) -> String {
    let parts: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() <= 3 { p.to_string() } else { parts[parts.len() - 3..].join("/") }
}

/// Règles (clé → exemples) qu'une cause du digest justifie.
fn found_for(cause: &Value) -> BTreeMap<&'static str, Vec<String>> {
    let id = cause["id"].as_str().unwrap_or("");
    let items = cause["items"].as_array().cloned().unwrap_or_default();
    let target = |it: &Value| it["target"].as_str().unwrap_or("").to_string();
    let mut out: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    let mut add = |key: &'static str, item: Option<String>| {
        let v = out.entry(key).or_default();
        if let Some(i) = item.filter(|i| !i.is_empty() && !v.contains(i)) {
            v.push(i);
        }
    };
    match id {
        "failed" => {
            for f in items.iter().filter_map(|it| it["error"].as_str().and_then(avoidable)) {
                add(f.key, f.item);
            }
        }
        "repeats" => {
            for it in &items {
                let t = target(it);
                let example = match it["name"].as_str() {
                    Some("Read") if durable(&t) => Some(short_path(&t)),
                    Some("Bash") if durable(&t) => Some(digest::command_head(&t)),
                    _ => None,
                };
                add("repeat", example);
            }
        }
        "big-results" => {
            for it in &items {
                match it["name"].as_str() {
                    Some("Bash") => add("big-cmd", Some(digest::command_head(&target(it)))),
                    Some("Read") => add("big-read", None),
                    _ => {}
                }
            }
        }
        _ if id.starts_with("tool:poll:") => add("poll", None),
        _ => {}
    }
    out
}

fn claude_md(conn: &Connection, sid: &str) -> Result<(PathBuf, i64, String)> {
    let (root, repo_id, name): (String, i64, String) = conn.query_row(
        "SELECT r.root, r.id, r.name FROM sessions s JOIN cwd_map c ON c.cwd = s.cwd JOIN repos r ON r.id = c.repo_id WHERE s.id = ?1",
        params![sid],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if !Path::new(&root).is_dir() {
        anyhow::bail!("The project folder `{root}` cannot receive rules because it no longer exists.");
    }
    Ok((Path::new(&root).join("CLAUDE.md"), repo_id, name))
}

/// Le bloc DRIFTy actuel : règles reconnues (clé → exemples) et lignes inconnues gardées telles quelles.
struct Block {
    rules: BTreeMap<String, Vec<String>>,
    foreign: Vec<String>,
}

fn backticked(s: &str) -> Vec<String> {
    s.split('`').skip(1).step_by(2).map(str::to_string).collect()
}

fn parse_block(text: &str) -> Block {
    let mut b = Block { rules: BTreeMap::new(), foreign: vec![] };
    let (Some(a), Some(z)) = (text.find(START), text.find(END)) else { return b };
    for line in text[a..z].lines().filter_map(|l| l.strip_prefix("- ")) {
        // Ligne étiquetée : `… <!-- k:clé -->`.
        if let Some((body, rest)) = line.rsplit_once("<!-- k:") {
            let key = rest.trim_end_matches("-->").trim();
            if let Some(k) = kind(key) {
                let items = if k.list { body.rsplit_once(" : ").map(|(_, l)| backticked(l)).unwrap_or_default() } else { vec![] };
                b.rules.insert(key.to_string(), items);
                continue;
            }
        }
        // Ligne écrite avant les clés : rattachée à son type si on la reconnaît.
        match legacy(line.trim()) {
            Some((key, items)) => {
                let v = b.rules.entry(key.to_string()).or_default();
                for i in items {
                    if !v.contains(&i) {
                        v.push(i);
                    }
                }
            }
            None => b.foreign.push(line.to_string()),
        }
    }
    b
}

/// Formulations des versions précédentes (avant les clés) → type de règle et exemples.
fn legacy(line: &str) -> Option<(&'static str, Vec<String>)> {
    if let Some(k) = CATALOGUE.iter().find(|k| render(k.key, &[]) == line) {
        return Some((k.key, vec![]));
    }
    let after = |sep: &str| line.rsplit_once(sep).map(|(_, l)| backticked(l)).unwrap_or_default();
    if line.starts_with("La commande `") && line.contains("n'existe pas dans cet environnement") {
        return Some(("missing-cmd", backticked(line).into_iter().take(1).collect()));
    }
    if line.starts_with("Ces commandes ont produit des sorties énormes") {
        return Some(("big-cmd", after(" au lieu de les lire en entier : ")));
    }
    if line.starts_with("Ne relis pas un fichier inchangé") {
        return Some(("repeat", after("pour rien : ")));
    }
    if line.starts_with("Ne relance pas une commande dont le résultat") {
        return Some(("repeat", line.rsplit_once('(').map(|(_, l)| backticked(l)).unwrap_or_default()));
    }
    if line.starts_with("Filtre les sorties d'outil volumineuses") {
        return Some(("big-cmd", vec![]));
    }
    None
}

/// Fusion : exemples nouveaux d'abord, puis anciens, sans doublon, plafonnés.
fn merge(old: &[String], new: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for i in new.iter().chain(old) {
        if !out.contains(i) {
            out.push(i.clone());
        }
    }
    out.truncate(MAX_ITEMS);
    out
}

fn line(key: &str, items: &[String]) -> String {
    format!("- {} <!-- k:{key} -->", render(key, items))
}

pub fn propose(conn: &Connection, sid: &str, cause_id: &str) -> Result<Value> {
    if !is_fixable(cause_id) {
        anyhow::bail!("The cause `{cause_id}` cannot be optimised because no concrete rule exists for it.");
    }
    let d = digest::digest(conn, sid)?;
    let cause = d["causes"]
        .as_array()
        .and_then(|cs| cs.iter().find(|c| c["id"] == cause_id))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("The cause `{cause_id}` cannot be optimised because it is not in this conversation."))?;
    let (path, _, name) = claude_md(conn, sid)?;
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let block = parse_block(&text);
    let mut entries = Vec::new();
    for (key, items) in found_for(&cause) {
        let (status, merged) = match block.rules.get(key) {
            None => ("new", merge(&[], &items)),
            Some(old) => {
                let m = merge(old, &items);
                (if &m == old { "present" } else { "updated" }, m)
            }
        };
        entries.push(json!({"key": key, "status": status, "text": render(key, &merged)}));
    }
    let after = {
        let mut keys: Vec<&str> = block.rules.keys().map(String::as_str).collect();
        for e in &entries {
            let k = e["key"].as_str().unwrap_or("");
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
        keys.len()
    };
    Ok(json!({
        "cause": cause_id,
        "project": name,
        "path": path,
        "file_exists": path.exists(),
        "entries": entries,
        "block_rules": after,
        "block_max": CATALOGUE.len(),
        "foreign": block.foreign.len(),
    }))
}

/// Réécrit le bloc DRIFTy : une ligne par règle du catalogue, dans l'ordre du
/// catalogue ; les lignes inconnues (écrites à la main) sont gardées à la fin.
/// Réécrit le bloc DRIFTy du texte (un seul point d'écriture) : une ligne par règle,
/// dans l'ordre du catalogue, puis les lignes écrites à la main. Le reste du fichier ne bouge pas.
fn rewrite(text: &str, block: &Block) -> String {
    let mut body = String::new();
    for k in CATALOGUE.iter().filter(|k| block.rules.contains_key(k.key)) {
        body += &line(k.key, &block.rules[k.key]);
        body.push('\n');
    }
    for f in &block.foreign {
        body += &format!("- {f}\n");
    }
    let new_block = format!("{START}\n{HEADING}\n\n{body}{END}");
    match (text.find(START), text.find(END)) {
        (Some(a), Some(z)) => format!("{}{}{}", &text[..a], new_block, &text[z + END.len()..]),
        _ => {
            let sep = if text.is_empty() || text.ends_with("\n\n") { "" } else if text.ends_with('\n') { "\n" } else { "\n\n" };
            format!("{text}{sep}{new_block}\n")
        }
    }
}

/// Les règles du bloc DRIFTy d'un texte : (clé, texte affiché), dans l'ordre du catalogue.
pub fn block_rules(text: &str) -> Vec<(String, String)> {
    let block = parse_block(text);
    CATALOGUE.iter().filter(|k| block.rules.contains_key(k.key)).map(|k| (k.key.to_string(), render(k.key, &block.rules[k.key]))).collect()
}

/// Le texte sans la règle `key` dans son bloc DRIFTy.
pub fn without_rule(text: &str, key: &str) -> Result<String> {
    let mut block = parse_block(text);
    if block.rules.remove(key).is_none() {
        anyhow::bail!("The rule `{key}` cannot be removed because it is not in the DRIFTy block.");
    }
    Ok(rewrite(text, &block))
}

pub fn apply(conn: &Connection, sid: &str, cause_id: &str) -> Result<Value> {
    let p = propose(conn, sid, cause_id)?;
    let changed: Vec<&Value> = p["entries"].as_array().into_iter().flatten().filter(|e| e["status"] != "present").collect();
    if changed.is_empty() {
        return Ok(json!({"changed": 0, "path": p["path"]}));
    }
    let d = digest::digest(conn, sid)?;
    let cause = d["causes"].as_array().and_then(|cs| cs.iter().find(|c| c["id"] == cause_id)).cloned().unwrap_or(Value::Null);
    let (path, repo_id, _) = claude_md(conn, sid)?;
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut block = parse_block(&text);
    for (key, items) in found_for(&cause) {
        let merged = merge(block.rules.get(key).map(Vec::as_slice).unwrap_or(&[]), &items);
        block.rules.insert(key.to_string(), merged);
    }
    std::fs::write(&path, rewrite(&text, &block))?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    let texts: Vec<&Value> = changed.iter().map(|e| &e["text"]).collect();
    conn.execute(
        "INSERT INTO fixes(ts, repo_id, session_id, cause, rules, path) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![now, repo_id, sid, cause_id, serde_json::to_string(&texts)?, path.to_string_lossy()],
    )?;
    Ok(json!({"changed": changed.len(), "path": path}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avoidable_vs_normal_failure() {
        assert!(avoidable("Exit code 1\nsrc/a.cpp:3: error: expected ';'").is_none());
        assert!(avoidable("The user doesn't want to take this action").is_none());
        assert_eq!(avoidable("Exit code 1 head: it.chk: No such file or directory").unwrap().key, "path");
        assert_eq!(avoidable("This agent is isolated in the worktree /x, but").unwrap().key, "worktree");
        assert_eq!(missing_command("(eval):1: rg: command not found").as_deref(), Some("rg"));
        assert_eq!(missing_command("(eval):4: command not found: fd").as_deref(), Some("fd"));
        assert_eq!(missing_command("(eval):28: command not found: 4"), None);
        assert_eq!(avoidable("(eval):4: command not found: static").unwrap().key, "code-shell");
        assert_eq!(avoidable("(eval):1: command not found: timeout").unwrap(), Found { key: "missing-cmd", item: Some("timeout".into()) });
    }

    #[test]
    fn block_roundtrip_and_legacy() {
        let safety = render("safety", &[]);
        let t = format!(
            "# X\n\n{START}\n{HEADING}\n\n- {safety}\n- La commande `timeout` n'existe pas dans cet environnement : ne l'utilise pas.\n{}\n- note perso\n{END}\n",
            line("big-cmd", &["make a".into(), "cargo b".into()])
        );
        let b = parse_block(&t);
        assert!(b.rules.contains_key("safety"));
        assert_eq!(b.rules["missing-cmd"], vec!["timeout".to_string()]);
        assert_eq!(b.rules["big-cmd"], vec!["make a".to_string(), "cargo b".to_string()]);
        assert_eq!(b.foreign, vec!["note perso".to_string()]);
        let old = parse_block(&format!("{START}\n- Ces commandes ont produit des sorties énormes : filtre-les (`| tail -n 40`) au lieu de les lire en entier : `sed -n`, `grep -a`.\n- Ne relis pas un fichier et ne relance pas une commande dont le résultat est déjà dans le contexte, sauf si quelque chose a changé.\n{END}"));
        assert_eq!(old.rules["big-cmd"], vec!["sed -n".to_string(), "grep -a".to_string()]);
        assert!(old.rules.contains_key("repeat") && old.foreign.is_empty());
    }

    #[test]
    fn merge_is_bounded() {
        let old: Vec<String> = (0..5).map(|i| format!("o{i}")).collect();
        let m = merge(&old, &["n1".into(), "o0".into()]);
        assert_eq!(m.len(), MAX_ITEMS);
        assert_eq!(m[0], "n1");
    }
}
