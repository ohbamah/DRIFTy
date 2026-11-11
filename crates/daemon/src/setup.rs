//! Script de mise en place d'un nouveau worktree (comme Conductor) : écrit dans les
//! réglages du projet, proposé par l'IA d'après le dépôt, lancé à la création de
//! chaque worktree avant le premier message de l'agent.

use anyhow::Result;
use axum::extract::{Path, State};
use axum::Json;
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::{commands, improve, repos, with_conn, ApiResult, App};
use drifty_core::db;

pub const SYSTEM: &str = "On te donne le résumé d'un dépôt de code et la liste des fichiers ignorés par git présents dans son \
dossier principal. Écris le script bash lancé dans un NOUVEAU worktree git de ce dépôt pour le rendre prêt à travailler : \
copier depuis le dossier principal ($DRIFTY_ROOT_PATH) les fichiers non versionnés nécessaires (.env, clés, config locale), \
installer les dépendances (bundle install, npm ci, uv sync…), préparer ce qui doit l'être. Jamais de serveur lancé, rien \
d'interactif, rien d'écrit hors du worktree. Le script est exécuté par `bash -e` dans le worktree ; protège les copies \
facultatives (`[ -f \"$DRIFTY_ROOT_PATH/.env\" ] && cp …`). Variables : $DRIFTY_ROOT_PATH (dossier principal), \
$DRIFTY_WORKSPACE_NAME (nom du worktree), $PORT (port du worktree). Si un conductor.json existe, reprends son script \
setup. Sous-modules git (listés dans le résumé) : un worktree neuf a leurs dossiers VIDES ; s'ils sont \
déclarés dans .gitmodules, `git submodule update --init --recursive` ; un gitlink SANS .gitmodules ne se met pas à jour \
par git submodule : clone-le depuis le dossier principal puis place-le sur le commit attendu \
(`git clone -q \"$DRIFTY_ROOT_PATH/<chemin>\" <chemin> && git -C <chemin> checkout -q \"$(git rev-parse HEAD:<chemin>)\"`). \
Si on te donne le script ACTUEL, pars de lui : garde ce qui marche, et ses commentaires sont des consignes de \
l'utilisateur, à appliquer et à garder dans le script. Script court, commenté, sans rien d'inutile ; vide si rien n'est nécessaire. Réponds UNIQUEMENT par un objet JSON : \
{\"script\": \"...\", \"why\": \"une phrase\"}.";

/// Réglage du projet : `{script, configured}`, plus la dernière proposition de l'IA.
pub fn get(c: &Connection, repo_id: i64) -> Result<Value> {
    Ok(db::setting(c, &format!("setup:{repo_id}"))?
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or(json!({"script": "", "configured": false})))
}

/// Script à lancer à la création d'un worktree (seulement s'il a été enregistré).
pub fn script_for(c: &Connection, repo_id: i64) -> Result<Option<String>> {
    let v = get(c, repo_id)?;
    Ok((v["configured"] == true).then(|| v["script"].as_str().unwrap_or("").trim().to_string()).filter(|s| !s.is_empty()))
}

/// Résumé envoyé à l'IA : celui des commandes, plus les fichiers ignorés et un éventuel conductor.json.
fn summary(root: &str) -> String {
    let mut s = commands::repo_summary(root);
    let ignored = std::process::Command::new("git")
        .args(["-C", root, "ls-files", "--others", "--ignored", "--exclude-standard", "--directory"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().take(80).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default();
    s += &format!("Fichiers ignorés par git présents dans le dossier principal :\n{ignored}\n");
    s += &submodules(root);
    if let Ok(c) = std::fs::read_to_string(std::path::Path::new(root).join("conductor.json")) {
        s += &format!("conductor.json :\n{}\n", c.chars().take(3000).collect::<String>());
    }
    s
}

/// Sous-modules (gitlinks) du dépôt, avec leur déclaration dans .gitmodules s'il y en a une :
/// un worktree neuf les a vides, et un gitlink non déclaré échappe à `git submodule`.
fn submodules(root: &str) -> String {
    let links = std::process::Command::new("git")
        .args(["-C", root, "ls-files", "--stage"])
        .output()
        .map(|o| gitlinks(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default();
    if links.is_empty() {
        return String::new();
    }
    let declared = std::fs::read_to_string(std::path::Path::new(root).join(".gitmodules")).unwrap_or_default();
    let mut s = String::from("Sous-modules git (gitlinks) :\n");
    for l in links {
        let how = if declared.contains(&format!("path = {l}")) { "déclaré dans .gitmodules" } else { "SANS entrée dans .gitmodules" };
        s += &format!("- {l} ({how})\n");
    }
    s
}

/// Chemins des gitlinks dans la sortie de `git ls-files --stage` (mode 160000).
fn gitlinks(stage: &str) -> Vec<String> {
    stage.lines().filter(|l| l.starts_with("160000 ")).filter_map(|l| l.split_once('\t')).map(|(_, p)| p.to_string()).collect()
}

/// Message envoyé à l'IA : le dépôt, et le script actuel du champ s'il n'est pas vide.
fn request(summary: &str, current: &str) -> String {
    let current = current.trim();
    if current.is_empty() {
        return summary.to_string();
    }
    format!("{summary}\nScript ACTUEL (à reprendre ; ses commentaires sont des consignes de l'utilisateur) :\n```bash\n{current}\n```\n")
}

fn parse(text: &str) -> Result<Value> {
    let (Some(a), Some(b)) = (text.find('{'), text.rfind('}')) else {
        anyhow::bail!("The setup script cannot be proposed because the answer holds no JSON.");
    };
    let v: Value = serde_json::from_str(&text[a..=b])?;
    Ok(json!({"script": v["script"].as_str().unwrap_or("").trim(), "why": v["why"].as_str().unwrap_or("")}))
}

/// Démarre le script d'un worktree tout neuf ; rend l'identifiant de l'exécution.
pub fn start(app: &App, repo_root: &str, cwd: &str, worktree: &str, script: &str, env: &[(String, String)]) -> Result<String> {
    let dir = drifty_core::home().join(".drifty").join("setup");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{}.sh", worktree.replace(['/', ' '], "_")));
    std::fs::write(&file, script)?;
    let mut env = env.to_vec();
    let port = env.iter().find(|(k, _)| k == "PORT").map(|(_, v)| v.clone()).unwrap_or_default();
    for (k, v) in [("DRIFTY_ROOT_PATH", repo_root), ("DRIFTY_WORKSPACE_NAME", worktree)] {
        env.push((k.into(), v.into()));
    }
    // Les scripts écrits pour Conductor marchent tels quels.
    for (k, v) in [("CONDUCTOR_ROOT_PATH", repo_root), ("CONDUCTOR_WORKSPACE_NAME", worktree), ("CONDUCTOR_PORT", port.as_str())] {
        env.push((k.into(), v.into()));
    }
    app.jobs.start(cwd, &format!("bash -e {}", commands::shell_quote(&file.to_string_lossy())), &env)
}

// ------------------------------------------------------------ routes

pub async fn route_get(State(app): State<App>, Path(id): Path<i64>) -> ApiResult {
    with_conn(&app, move |c| get(c, id)).await
}

pub async fn route_set(State(app): State<App>, Path(id): Path<i64>, Json(v): Json<Value>) -> ApiResult {
    with_conn(&app, move |c| {
        let mut cur = get(c, id)?;
        cur["script"] = json!(v["script"].as_str().unwrap_or("").trim());
        cur["configured"] = json!(true);
        db::set_setting(c, &format!("setup:{id}"), &cur.to_string())?;
        Ok(cur)
    })
    .await
}

/// Proposition de l'IA ; gardée comme proposition (sans écraser un script enregistré).
/// Corps : `{current}`, le contenu du champ (enregistré ou non) d'où l'IA repart.
pub async fn route_analyze(State(app): State<App>, Path(id): Path<i64>, Json(body): Json<Value>) -> ApiResult {
    let current = body["current"].as_str().unwrap_or("").to_string();
    let v = with_conn(&app, move |c| {
        let (root, slot): (String, Option<String>) = c.query_row("SELECT root, slot FROM repos WHERE id = ?1", rusqlite::params![id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let slot = slot.or(db::default_slot(c)?);
        Ok(json!([request(&summary(&root), &current), repos::account_dir(c, slot.as_deref())?]))
    })
    .await?
    .0;
    let model = std::env::var("DRIFTY_IMPROVE_MODEL").unwrap_or_else(|_| "sonnet".into());
    let r = improve::oneshot(SYSTEM, v[0].as_str().unwrap_or(""), v[1].as_str(), &model, "The setup script cannot be proposed").await?;
    let found = parse(&r.text)?;
    let (i, o, cost) = (r.input_tokens, r.output_tokens, r.cost_usd);
    let keep = found.clone();
    let _ = with_conn(&app, move |c| {
        drifty_core::ledger::record_spend(c, "setup-analysis", None, i, o, cost)?;
        let mut cur = get(c, id)?;
        cur["proposal"] = keep;
        db::set_setting(c, &format!("setup:{id}"), &cur.to_string())?;
        Ok(json!({}))
    })
    .await?;
    Ok(Json(found))
}

#[cfg(test)]
mod tests {
    #[test]
    fn answer_is_parsed() {
        let v = super::parse("Voici :\n{\"script\": \"cp -n a b\\n\", \"why\": \"copie\"}").unwrap();
        assert_eq!(v["script"], "cp -n a b");
        assert!(super::parse("rien").is_err());
    }

    #[test]
    fn gitlinks_are_found() {
        let stage = "100644 aa 0\tMakefile\n160000 1baab50 0\thopt\n160000 cc 0\tvendor/x y\n";
        assert_eq!(super::gitlinks(stage), ["hopt", "vendor/x y"]);
    }

    #[test]
    fn current_script_reaches_the_request() {
        assert_eq!(super::request("dépôt", "  \n"), "dépôt");
        let r = super::request("dépôt", "# Il faut utiliser le gitmodule hopt\nmake dev\n");
        assert!(r.contains("# Il faut utiliser le gitmodule hopt\nmake dev") && r.contains("consignes"));
    }
}
