//! Panneau GitHub d'une conversation : branche (commit, push, pull), PR de la branche
//! (CI, relecteurs, commentaires, fusion). Tout passe par `git` et `gh`, lancés dans le
//! dossier de la conversation ; la PR est toujours retrouvée côté serveur par la branche,
//! jamais désignée par la page.

use anyhow::{bail, Result};
use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::process::Command;

use crate::{review, review_dir, ApiResult, App, ReviewTarget};

const PR_FIELDS: &str = "number,title,url,state,isDraft,reviewDecision,mergeable,mergeStateStatus,statusCheckRollup,baseRefName";

fn run(dir: &str, prog: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(prog)
        .current_dir(dir)
        .args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| anyhow::anyhow!("`{prog}` cannot run: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = if err.trim().is_empty() { String::from_utf8_lossy(&out.stdout) } else { err };
        bail!("`{prog} {}` failed: {}", args.iter().take(2).copied().collect::<Vec<_>>().join(" "), err.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn ok(dir: &str, prog: &str, args: &[&str]) -> Option<String> {
    run(dir, prog, args).ok().map(|s| s.trim().to_string())
}

fn branch(dir: &str) -> String {
    ok(dir, "git", &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default()
}

/// `(retard, avance)` de HEAD par rapport à `other`.
fn counts(dir: &str, other: &str) -> Option<(i64, i64)> {
    let s = ok(dir, "git", &["rev-list", "--left-right", "--count", &format!("{other}...HEAD")])?;
    let mut it = s.split_whitespace().filter_map(|x| x.parse().ok());
    Some((it.next()?, it.next()?))
}

/// Numéro d'exécution GitHub Actions dans l'adresse d'un check (`…/actions/runs/<id>/job/…`).
fn run_id(url: &str) -> Option<u64> {
    let rest = url.split("/actions/runs/").nth(1)?;
    rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

fn lower(v: &Value) -> String {
    v.as_str().unwrap_or("").to_ascii_lowercase()
}

/// Checks de la PR, dans une forme unique (CheckRun et StatusContext confondus).
fn checks(rollup: &Value) -> Vec<Value> {
    rollup
        .as_array()
        .map(|a| {
            a.iter()
                .map(|c| {
                    if c["__typename"] == "StatusContext" {
                        let state = lower(&c["state"]);
                        let pending = state == "pending" || state == "expected";
                        json!({"name": c["context"], "workflow": "", "status": if pending { "pending" } else { "done" },
                               "conclusion": if pending { String::new() } else { state }, "url": c["targetUrl"], "run_id": null,
                               "started_at": c["startedAt"], "completed_at": null})
                    } else {
                        let url = c["detailsUrl"].as_str().unwrap_or("");
                        json!({"name": c["name"], "workflow": c["workflowName"], "status": if c["status"] == "COMPLETED" { "done" } else { "pending" },
                               "conclusion": lower(&c["conclusion"]), "url": url, "run_id": run_id(url),
                               "started_at": c["startedAt"], "completed_at": c["completedAt"]})
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

const THREAD_QUERY: &str = "query($owner:String!,$repo:String!,$n:Int!){
  viewer { login avatarUrl(size:64) }
  repository(owner:$owner,name:$repo){ viewerPermission pullRequest(number:$n){
    author { login avatarUrl(size:64) }
    headRefName commits { totalCount } viewerCanMergeAsAdmin
    reviewRequests(first:30){ nodes { requestedReviewer {
      ... on User { login avatarUrl(size:64) } ... on Bot { login avatarUrl(size:64) } ... on Team { name avatarUrl(size:64) } ... on Mannequin { login avatarUrl(size:64) } } } }
    latestReviews(first:30){ nodes { author { login avatarUrl(size:64) } state } }
    comments(first:100){ nodes { author { login avatarUrl(size:64) } body bodyHTML createdAt url isMinimized minimizedReason } }
    reviews(first:100){ nodes { author { login avatarUrl(size:64) } body bodyHTML state submittedAt url } }
    reviewThreads(first:100){ nodes { id viewerCanResolve viewerCanUnresolve isResolved isOutdated path line originalLine resolvedBy { login }
      comments(first:50){ nodes { author { login avatarUrl(size:64) } body bodyHTML createdAt url diffHunk } } } }
  } } }";

fn person(a: &Value) -> Value {
    json!({"login": a["login"].as_str().or(a["name"].as_str()).unwrap_or("ghost"), "avatar": a["avatarUrl"]})
}

fn nodes(v: &Value) -> impl Iterator<Item = &Value> {
    v["nodes"].as_array().into_iter().flatten()
}

/// Fil de la PR, rendu par GitHub (HTML) : commentaires, revues, fils de code (résolus ou non), par date ;
/// plus les relecteurs (demandés et ayant relu) et l'utilisateur connecté.
fn timeline(dir: &str, n: i64) -> Result<Value> {
    let raw = run(dir, "gh", &["api", "graphql", "-F", "owner={owner}", "-F", "repo={repo}", "-F", &format!("n={n}"), "-f", &format!("query={THREAD_QUERY}")])?;
    let v: Value = serde_json::from_str(&raw)?;
    let pr = &v["data"]["repository"]["pullRequest"];
    let mut items: Vec<Value> = Vec::new();
    for c in nodes(&pr["comments"]) {
        items.push(json!({"kind": "comment", "who": person(&c["author"]), "body": c["body"], "html": c["bodyHTML"], "at": c["createdAt"], "url": c["url"],
                          "hidden": c["isMinimized"], "hidden_why": lower(&c["minimizedReason"])}));
    }
    for r in nodes(&pr["reviews"]) {
        let state = lower(&r["state"]);
        // Une revue sans texte ni verdict n'est que l'enveloppe de commentaires de code (montrés dans leur fil).
        if r["body"].as_str().unwrap_or("").trim().is_empty() && (state == "commented" || state == "pending") {
            continue;
        }
        items.push(json!({"kind": "review", "who": person(&r["author"]), "body": r["body"], "html": r["bodyHTML"], "state": state, "at": r["submittedAt"], "url": r["url"]}));
    }
    for t in nodes(&pr["reviewThreads"]) {
        let comments: Vec<Value> = nodes(&t["comments"])
            .map(|c| json!({"who": person(&c["author"]), "body": c["body"], "html": c["bodyHTML"], "at": c["createdAt"], "url": c["url"]}))
            .collect();
        let first = nodes(&t["comments"]).next();
        items.push(json!({"kind": "thread", "path": t["path"], "line": if t["line"].is_null() { t["originalLine"].clone() } else { t["line"].clone() },
                          "resolved": t["isResolved"], "outdated": t["isOutdated"], "resolved_by": t["resolvedBy"]["login"],
                          "id": t["id"], "can_resolve": t["viewerCanResolve"], "can_unresolve": t["viewerCanUnresolve"],
                          "hunk": first.map_or(Value::Null, |c| c["diffHunk"].clone()), "at": first.map_or(Value::Null, |c| c["createdAt"].clone()),
                          "comments": comments}));
    }
    items.sort_by(|a, b| a["at"].as_str().unwrap_or("").cmp(b["at"].as_str().unwrap_or("")));
    // Relecteurs : demandés (en attente) puis ceux qui ont relu, avec leur dernier verdict.
    let mut reviewers: Vec<Value> = nodes(&pr["reviewRequests"]).map(|r| json!({"who": person(&r["requestedReviewer"]), "state": "requested"})).collect();
    for r in nodes(&pr["latestReviews"]) {
        let who = person(&r["author"]);
        if !reviewers.iter().any(|x| x["who"]["login"] == who["login"]) {
            reviewers.push(json!({"who": who, "state": lower(&r["state"])}));
        }
    }
    Ok(json!({"items": items, "reviewers": reviewers, "viewer": person(&v["data"]["viewer"]), "author": person(&pr["author"]),
              "head": pr["headRefName"], "commits": pr["commits"]["totalCount"], // Contournement : protection de branche (viewerCanMergeAsAdmin) ou administrateur du dépôt, que les
              // rulesets laissent passer (GitHub montre alors sa case « bypass rules » ; il tranche à la fusion).
              "can_admin": pr["viewerCanMergeAsAdmin"] == true || v["data"]["repository"]["viewerPermission"] == "ADMIN"}))
}

/// Ce qui autorise la fusion, lu en UN point (l'affichage et l'action le partagent) :
/// `ok` ; `confirm` (checks en échec ou en cours, sans règle qui bloque : fusion après
/// confirmation) ; `rules` (une règle du dépôt bloque : contournement par un
/// administrateur, ou fusion automatique) ; `conflict` et `draft` (aucune fusion possible).
fn gate(p: &Value, checks: &[Value]) -> Value {
    let merge_state = lower(&p["mergeStateStatus"]);
    let failed = checks.iter().filter(|c| c["status"] == "done" && !matches!(c["conclusion"].as_str(), Some("success" | "skipped" | "neutral" | "cancelled"))).count();
    let pending = checks.iter().filter(|c| c["status"] == "pending").count();
    let mut reasons: Vec<String> = Vec::new();
    if p["isDraft"] == true {
        return json!({"level": "draft", "reasons": ["La PR est un brouillon."]});
    }
    if lower(&p["mergeable"]) == "conflicting" || merge_state == "dirty" {
        return json!({"level": "conflict", "reasons": ["La branche a des conflits avec la base."]});
    }
    match lower(&p["reviewDecision"]).as_str() {
        "review_required" => reasons.push("Une relecture approuvée est requise.".into()),
        "changes_requested" => reasons.push("Un relecteur demande des changements.".into()),
        _ => {}
    }
    if failed > 0 {
        reasons.push(format!("{failed} check{} en échec.", if failed > 1 { "s" } else { "" }));
    }
    if pending > 0 {
        reasons.push(format!("{pending} check{} en cours.", if pending > 1 { "s" } else { "" }));
    }
    if merge_state == "behind" {
        reasons.push("La branche doit être à jour avec la base.".into());
    }
    let level = if merge_state == "blocked" || merge_state == "behind" {
        if reasons.is_empty() {
            reasons.push("Une règle du dépôt bloque la fusion.".into());
        }
        "rules"
    } else if failed > 0 || pending > 0 {
        // Sans règle qui bloque, les relectures manquantes ne sont qu'une information.
        reasons.retain(|r| r.contains("check"));
        "confirm"
    } else {
        "ok"
    };
    json!({"level": level, "reasons": reasons})
}

/// Pile de PR empilées autour de la branche : les PR dont elle part (vers la base), puis
/// celles qui partent d'elle. Du bas (la plus proche de la base) vers le haut ; vide si seule.
fn stack(dir: &str, br: &str) -> Value {
    let Some(raw) = ok(dir, "gh", &["pr", "list", "--state", "open", "--limit", "200", "--json", "number,title,headRefName,baseRefName,isDraft,url"]) else {
        return json!([]);
    };
    let Ok(Value::Array(prs)) = serde_json::from_str::<Value>(&raw) else { return json!([]) };
    let head = |b: &str| prs.iter().find(|p| p["headRefName"] == b);
    let Some(me) = head(br) else { return json!([]) };
    let mut chain = vec![me.clone()];
    // Vers le bas : la PR dont la tête est ma base, etc.
    let mut base = me["baseRefName"].as_str().unwrap_or("").to_string();
    while let Some(p) = head(&base).filter(|p| !chain.iter().any(|c| c["number"] == p["number"])) {
        chain.insert(0, p.clone());
        base = p["baseRefName"].as_str().unwrap_or("").to_string();
    }
    // Vers le haut : une PR qui part de ma tête (la première s'il y en a plusieurs).
    let mut top = br.to_string();
    while let Some(p) = prs.iter().find(|p| p["baseRefName"] == top.as_str() && !chain.iter().any(|c| c["number"] == p["number"])) {
        chain.push(p.clone());
        top = p["headRefName"].as_str().unwrap_or("").to_string();
    }
    if chain.len() < 2 {
        return json!([]);
    }
    Value::Array(
        chain
            .iter()
            .map(|p| json!({"number": p["number"], "title": p["title"], "head": p["headRefName"], "base": p["baseRefName"], "draft": p["isDraft"], "url": p["url"], "current": p["headRefName"] == br}))
            .collect(),
    )
}

/// PR ouverte (ou la plus récente) de la branche ; `None` s'il n'y en a pas.
fn pr_view(dir: &str, branch: &str) -> Result<Option<Value>> {
    match run(dir, "gh", &["pr", "view", branch, "--json", PR_FIELDS]) {
        Ok(s) => Ok(Some(serde_json::from_str(&s)?)),
        Err(e) if e.to_string().contains("no pull requests found") => Ok(None),
        Err(e) => Err(e),
    }
}

fn pr_number(dir: &str) -> Result<i64> {
    let b = branch(dir);
    pr_view(dir, &b)?
        .and_then(|p| p["number"].as_i64())
        .ok_or_else(|| anyhow::anyhow!("The pull request cannot be found because the branch `{b}` has none."))
}

pub fn state(dir: &str) -> Result<Value> {
    if ok(dir, "git", &["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        bail!("The GitHub panel cannot open because `{dir}` is not a git repository.");
    }
    let br = branch(dir);
    let base = review::base_branch(dir);
    let dirty = run(dir, "git", &["status", "--porcelain"])?.lines().filter(|l| !l.is_empty()).count();
    // Base à jour (panneau ouvert à la demande : un `fetch` de la seule branche de base).
    if br != base {
        let _ = run(dir, "git", &["fetch", "--quiet", "origin", &base]);
    }
    let upstream = ok(dir, "git", &["rev-parse", "--abbrev-ref", "@{u}"]);
    let (unpulled, unpushed) = upstream.as_deref().and_then(|u| counts(dir, u)).unwrap_or((0, 0));
    let local = branch_status(dir);
    let (base_ref, behind, ahead, conflicts) = (local["base_ref"].as_str().unwrap_or(&base).to_string(), local["behind"].as_i64().unwrap_or(0), local["ahead"].as_i64().unwrap_or(0), local["conflicts"].clone());
    let has_gh = ok(dir, "gh", &["--version"]).is_some();
    let (pr, gh_error) = if !has_gh || br == base || br == "HEAD" {
        (Value::Null, Value::Null)
    } else {
        match pr_view(dir, &br) {
            Ok(Some(p)) => {
                let checks = checks(&p["statusCheckRollup"]);
                let n = p["number"].as_i64().unwrap_or(0);
                let t = timeline(dir, n)?;
                (
                    json!({"number": n, "title": p["title"], "url": p["url"], "state": lower(&p["state"]), "draft": p["isDraft"],
                           "review_decision": lower(&p["reviewDecision"]), "mergeable": lower(&p["mergeable"]), "merge_state": lower(&p["mergeStateStatus"]),
                           "base": p["baseRefName"], "head": t["head"], "commits": t["commits"], "author": t["author"], "viewer": t["viewer"],
                           "reviewers": t["reviewers"], "stack": stack(dir, &br), "gate": gate(&p, &checks), "can_admin": t["can_admin"], "checks": checks, "thread": t["items"]}),
                    Value::Null,
                )
            }
            Ok(None) => (Value::Null, Value::Null),
            Err(e) => (Value::Null, json!(e.to_string())),
        }
    };
    Ok(json!({
        "branch": br, "base": base, "base_ref": base_ref, "dirty": dirty, "upstream": upstream, "unpushed": unpushed, "unpulled": unpulled,
        "ahead": ahead, "behind": behind, "conflicts": conflicts, "has_gh": has_gh, "pr": pr, "gh_error": gh_error,
    }))
}

/// Logins proposés comme relecteurs : Copilot, puis les collaborateurs (à défaut, les assignables), sauf soi.
pub fn reviewers(dir: &str) -> Result<Value> {
    let me = ok(dir, "gh", &["api", "user", "--jq", ".login"]).unwrap_or_default();
    let list = ok(dir, "gh", &["api", "repos/{owner}/{repo}/collaborators?per_page=100", "--jq", ".[].login"])
        .or_else(|| ok(dir, "gh", &["api", "repos/{owner}/{repo}/assignees?per_page=100", "--jq", ".[].login"]))
        .unwrap_or_default();
    let mut out = vec![json!("@copilot")];
    out.extend(list.lines().map(str::trim).filter(|l| !l.is_empty() && *l != me).map(|l| json!(l)));
    Ok(Value::Array(out))
}

/// Fin du journal des étapes en échec d'une exécution de CI (pour la confier à Claude).
pub fn failed_log(dir: &str, run_no: u64) -> Result<String> {
    let log = run(dir, "gh", &["run", "view", &run_no.to_string(), "--log-failed"])?;
    let lines: Vec<&str> = log.lines().collect();
    let mut tail = lines[lines.len().saturating_sub(150)..].join("\n");
    if tail.len() > 14_000 {
        let mut cut = tail.len() - 14_000;
        while !tail.is_char_boundary(cut) {
            cut += 1;
        }
        tail = tail[cut..].to_string();
    }
    Ok(tail)
}

fn valid_login(l: &str) -> bool {
    l == "@copilot" || (!l.is_empty() && l.len() <= 80 && l.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/')) && !l.starts_with('-'))
}

#[derive(Deserialize)]
pub struct ActionReq {
    #[serde(flatten)]
    target: ReviewTarget,
    action: String,
    login: Option<String>,
    body: Option<String>,
    method: Option<String>,
    #[serde(default)]
    auto: bool,
    #[serde(default)]
    draft: bool,
    /// Fusion malgré des checks en échec ou en cours (confirmée par une case).
    #[serde(default)]
    force: bool,
    /// Contournement des règles du dépôt (`gh pr merge --admin`).
    #[serde(default)]
    admin: bool,
    run_id: Option<u64>,
    /// Fil de revue à résoudre / rouvrir (identifiant GraphQL).
    thread_id: Option<String>,
    /// PR de la pile vers laquelle basculer.
    number: Option<i64>,
}

pub fn act(dir: &str, a: &ActionReq) -> Result<Value> {
    match a.action.as_str() {
        "commit" => review::commit(dir, a.target.message.as_deref().unwrap_or("")),
        "push" => {
            let b = branch(dir);
            if ok(dir, "git", &["rev-parse", "--abbrev-ref", "@{u}"]).is_some() {
                run(dir, "git", &["push"])?;
            } else {
                run(dir, "git", &["push", "-u", "origin", &b])?;
            }
            Ok(json!({"pushed": b}))
        }
        "pull" => {
            run(dir, "git", &["pull", "--ff-only"])?;
            Ok(json!({"ok": true}))
        }
        "pr" => review::pull_request(dir, a.draft),
        "merge_local" => review::merge(dir),
        "review" => {
            let login = a.login.as_deref().unwrap_or("").trim();
            if !valid_login(login) {
                bail!("The review cannot be requested because `{login}` is not a GitHub login.");
            }
            let n = pr_number(dir)?;
            run(dir, "gh", &["pr", "edit", &n.to_string(), "--add-reviewer", login])?;
            Ok(json!({"requested": login}))
        }
        "comment" => {
            let body = a.body.as_deref().unwrap_or("").trim();
            if body.is_empty() {
                bail!("The comment cannot be posted because it is empty.");
            }
            let n = pr_number(dir)?;
            run(dir, "gh", &["pr", "comment", &n.to_string(), "--body", body])?;
            Ok(json!({"ok": true}))
        }
        "merge" => {
            let method = a.method.as_deref().unwrap_or("squash");
            if !matches!(method, "merge" | "squash" | "rebase") {
                bail!("The pull request cannot be merged because `{method}` is not merge, squash or rebase.");
            }
            let b = branch(dir);
            let p = pr_view(dir, &b)?.ok_or_else(|| anyhow::anyhow!("The pull request cannot be found because the branch `{b}` has none."))?;
            let g = gate(&p, &checks(&p["statusCheckRollup"]));
            let level = g["level"].as_str().unwrap_or("");
            let why = g["reasons"].as_array().map(|r| r.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")).unwrap_or_default();
            match level {
                "conflict" | "draft" => bail!("The pull request cannot be merged because: {why}"),
                "rules" if !a.auto && !a.admin => bail!("The pull request cannot be merged because a repository rule blocks it: {why}"),
                "confirm" if !a.auto && !a.force => bail!("The pull request cannot be merged without confirmation because: {why}"),
                _ => {}
            }
            if a.auto && a.admin {
                bail!("The pull request cannot be merged because automatic merge and rule bypass exclude each other.");
            }
            let n = p["number"].as_i64().unwrap_or(0).to_string();
            let flag = format!("--{method}");
            let mut args = vec!["pr", "merge", n.as_str(), flag.as_str()];
            if a.auto {
                args.push("--auto");
            }
            if a.admin {
                args.push("--admin");
            }
            run(dir, "gh", &args)?;
            Ok(json!({"method": method, "auto": a.auto, "admin": a.admin}))
        }
        "ready" => {
            let n = pr_number(dir)?;
            run(dir, "gh", &["pr", "ready", &n.to_string()])?;
            Ok(json!({"ok": true}))
        }
        "resolve" | "unresolve" => {
            let id = a.thread_id.as_deref().unwrap_or("");
            if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '=')) {
                bail!("The review thread cannot be changed because `{id}` is not a thread id.");
            }
            let m = if a.action == "resolve" { "resolveReviewThread" } else { "unresolveReviewThread" };
            let q = format!("mutation($id:ID!){{ {m}(input:{{threadId:$id}}) {{ thread {{ isResolved }} }} }}");
            run(dir, "gh", &["api", "graphql", "-F", &format!("id={id}"), "-f", &format!("query={q}")])?;
            Ok(json!({"ok": true}))
        }
        "checkout" => {
            let n = a.number.filter(|n| *n > 0).ok_or_else(|| anyhow::anyhow!("The pull request cannot be checked out because no number is given."))?;
            let dirty = run(dir, "git", &["status", "--porcelain", "--untracked-files=no"])?.lines().filter(|l| !l.is_empty()).count();
            if dirty > 0 {
                bail!("The pull request #{n} cannot be checked out because {dirty} tracked file{} would be overwritten (uncommitted changes).", if dirty > 1 { "s" } else { "" });
            }
            run(dir, "gh", &["pr", "checkout", &n.to_string()])?;
            Ok(json!({"branch": branch(dir)}))
        }
        "rerun" => {
            let id = a.run_id.ok_or_else(|| anyhow::anyhow!("The CI cannot be rerun because no run is named."))?;
            run(dir, "gh", &["run", "rerun", &id.to_string(), "--failed"])?;
            Ok(json!({"ok": true}))
        }
        other => bail!("The GitHub action `{other}` cannot run because it does not exist."),
    }
}

// ------------------------------------------------------------ routes

pub async fn get_state(State(app): State<App>, Query(t): Query<ReviewTarget>) -> ApiResult {
    let (dir, _) = review_dir(&app, &t).await?;
    Ok(Json(tokio::task::spawn_blocking(move || state(&dir)).await??))
}

pub async fn get_reviewers(State(app): State<App>, Query(t): Query<ReviewTarget>) -> ApiResult {
    let (dir, _) = review_dir(&app, &t).await?;
    Ok(Json(tokio::task::spawn_blocking(move || reviewers(&dir)).await??))
}

#[derive(Deserialize)]
pub struct LogQ {
    #[serde(flatten)]
    target: ReviewTarget,
    run: u64,
}

/// État LOCAL de la branche face à sa base (aucun accès réseau) : retard, avance, et
/// fichiers qui entreraient en conflit à un rebase (`git merge-tree`, rien n'est modifié).
pub fn branch_status(dir: &str) -> Value {
    let br = branch(dir);
    let base = review::base_branch(dir);
    let remote_base = format!("origin/{base}");
    let base_ref = if ok(dir, "git", &["rev-parse", "--verify", "--quiet", &remote_base]).is_some() { remote_base } else { base.clone() };
    let (behind, ahead) = if br == base || br == "HEAD" { (0, 0) } else { counts(dir, &base_ref).unwrap_or((0, 0)) };
    let mut conflicts: Vec<String> = Vec::new();
    if behind > 0 && ahead > 0 {
        if let Ok(o) = Command::new("git").current_dir(dir).args(["merge-tree", "--write-tree", "--name-only", "--no-messages", &base_ref, "HEAD"]).output() {
            // Code 1 = conflits : la 1re ligne est l'arbre, les suivantes les fichiers en conflit.
            if o.status.code() == Some(1) {
                conflicts = String::from_utf8_lossy(&o.stdout).lines().skip(1).filter(|l| !l.is_empty()).map(str::to_string).collect();
                conflicts.dedup();
            }
        }
    }
    json!({"branch": br, "base": base, "base_ref": base_ref, "behind": behind, "ahead": ahead, "conflicts": conflicts})
}

pub async fn get_branch(State(app): State<App>, Query(t): Query<ReviewTarget>) -> ApiResult {
    let (dir, _) = review_dir(&app, &t).await?;
    Ok(Json(tokio::task::spawn_blocking(move || branch_status(&dir)).await?))
}

pub async fn get_log(State(app): State<App>, Query(q): Query<LogQ>) -> ApiResult {
    let (dir, _) = review_dir(&app, &q.target).await?;
    let run = q.run;
    let log = tokio::task::spawn_blocking(move || failed_log(&dir, run)).await??;
    Ok(Json(json!({"log": log})))
}

pub async fn post_action(State(app): State<App>, Json(a): Json<ActionReq>) -> ApiResult {
    let (dir, _) = review_dir(&app, &a.target).await?;
    Ok(Json(tokio::task::spawn_blocking(move || act(&dir, &a)).await??))
}

#[cfg(test)]
mod tests {
    #[test]
    fn run_id_and_logins() {
        assert_eq!(super::run_id("https://github.com/o/r/actions/runs/123456/job/789"), Some(123456));
        assert_eq!(super::run_id("https://ci.example.com/x"), None);
        assert!(super::valid_login("@copilot") && super::valid_login("octo-cat") && super::valid_login("org/team"));
        assert!(!super::valid_login("--admin") && !super::valid_login("a b") && !super::valid_login(""));
    }

    #[test]
    fn merge_gate() {
        use serde_json::json;
        let ok = json!({"status": "done", "conclusion": "success"});
        let ko = json!({"status": "done", "conclusion": "failure"});
        let pr = |state: &str, mergeable: &str| json!({"mergeStateStatus": state, "mergeable": mergeable, "isDraft": false, "reviewDecision": ""});
        assert_eq!(super::gate(&pr("CLEAN", "MERGEABLE"), &[ok.clone()])["level"], "ok");
        assert_eq!(super::gate(&pr("UNSTABLE", "MERGEABLE"), &[ko.clone()])["level"], "confirm");
        assert_eq!(super::gate(&pr("BLOCKED", "MERGEABLE"), &[ko.clone()])["level"], "rules");
        assert_eq!(super::gate(&pr("BLOCKED", "MERGEABLE"), &[])["reasons"][0], "Une règle du dépôt bloque la fusion.");
        assert_eq!(super::gate(&pr("DIRTY", "CONFLICTING"), &[])["level"], "conflict");
    }

    #[test]
    fn checks_are_normalized() {
        let v = serde_json::json!([
            {"__typename": "CheckRun", "name": "test", "workflowName": "CI", "status": "IN_PROGRESS", "conclusion": "", "detailsUrl": "https://github.com/o/r/actions/runs/9/job/1"},
            {"__typename": "StatusContext", "context": "deploy", "state": "FAILURE", "targetUrl": "https://x"}
        ]);
        let c = super::checks(&v);
        assert_eq!(c[0]["status"], "pending");
        assert_eq!(c[0]["run_id"], 9);
        assert_eq!(c[1]["conclusion"], "failure");
    }
}
