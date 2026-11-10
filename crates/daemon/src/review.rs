//! Revue du worktree d'une conversation : état git, diff, commit, fusion dans la
//! branche de base, PR. Le dossier vient toujours de la conversation (chat ou
//! transcript), jamais d'un chemin fourni par la page.

use anyhow::Result;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

const MAX_DIFF: usize = 400_000;

fn git(dir: &str, args: &[&str]) -> Result<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output()?;
    if !out.status.success() {
        anyhow::bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn ok(dir: &str, args: &[&str]) -> Option<String> {
    git(dir, args).ok().map(|s| s.trim().to_string())
}

/// Branche de base du dépôt : celle d'origin/HEAD, sinon main, sinon master.
pub(crate) fn base_branch(dir: &str) -> String {
    if let Some(r) = ok(dir, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]) {
        if let Some(b) = r.strip_prefix("origin/") {
            return b.to_string();
        }
    }
    for b in ["main", "master"] {
        if ok(dir, &["rev-parse", "--verify", "--quiet", b]).is_some() {
            return b.to_string();
        }
    }
    "main".into()
}

/// Dossier du dépôt principal (où vit la branche de base).
fn main_root(dir: &str) -> Option<String> {
    let common = ok(dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    Some(common.trim_end_matches("/.git").to_string())
}

fn clip(s: String) -> (String, bool) {
    if s.len() <= MAX_DIFF {
        return (s, false);
    }
    let mut end = MAX_DIFF;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

pub fn state(dir: &str) -> Result<Value> {
    if !Path::new(dir).is_dir() {
        anyhow::bail!("The changes cannot be shown because the folder `{dir}` no longer exists.");
    }
    if ok(dir, &["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        anyhow::bail!("The changes cannot be shown because `{dir}` is not a git repository.");
    }
    let branch = ok(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let base = base_branch(dir);
    let (behind, ahead) = ok(dir, &["rev-list", "--left-right", "--count", &format!("{base}...HEAD")])
        .and_then(|s| {
            let mut it = s.split_whitespace().filter_map(|x| x.parse::<i64>().ok());
            Some((it.next()?, it.next()?))
        })
        .unwrap_or((0, 0));
    let status: Vec<Value> = git(dir, &["status", "--porcelain=v1", "-uall"])?
        .lines()
        .filter(|l| l.len() > 3)
        .map(|l| json!({"code": l[..2].trim(), "path": l[3..].trim_matches('"')}))
        .collect();
    // Modifications non commitées (fichiers suivis), puis fichiers nouveaux en entier.
    let mut work = git(dir, &["diff", "HEAD"]).unwrap_or_default();
    for s in status.iter().filter(|s| s["code"] == "??") {
        let p = s["path"].as_str().unwrap_or("");
        if let Ok(out) = Command::new("git").arg("-C").arg(dir).args(["diff", "--no-index", "--", "/dev/null", p]).output() {
            work += &String::from_utf8_lossy(&out.stdout);
        }
    }
    let (work, work_cut) = clip(work);
    let branch_diff = if branch != base && ahead > 0 { git(dir, &["diff", &format!("{base}...HEAD")]).unwrap_or_default() } else { String::new() };
    let (branch_diff, branch_cut) = clip(branch_diff);
    let commits: Vec<Value> = if ahead > 0 {
        git(dir, &["log", "--format=%h%x09%s", &format!("{base}..HEAD")])
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('\t').map(|(h, s)| json!({"hash": h, "subject": s})))
            .collect()
    } else {
        vec![]
    };
    let root = main_root(dir);
    Ok(json!({
        "dir": dir, "branch": branch, "base": base, "ahead": ahead, "behind": behind,
        "is_worktree": root.as_deref().is_some_and(|r| Path::new(r) != Path::new(dir)) && branch != base,
        "status": status, "work_diff": work, "work_cut": work_cut, "branch_diff": branch_diff, "branch_cut": branch_cut,
        "commits": commits, "has_gh": Command::new("gh").arg("--version").output().is_ok_and(|o| o.status.success()),
    }))
}

pub fn commit(dir: &str, message: &str) -> Result<Value> {
    let msg = message.trim();
    if msg.is_empty() {
        anyhow::bail!("The changes cannot be committed because the message is empty.");
    }
    git(dir, &["add", "-A"])?;
    if ok(dir, &["diff", "--cached", "--quiet"]).is_some() {
        anyhow::bail!("The changes cannot be committed because there is nothing to commit.");
    }
    git(dir, &["commit", "-m", msg])?;
    Ok(json!({"hash": ok(dir, &["rev-parse", "--short", "HEAD"])}))
}

/// Fusionne la branche du worktree dans la base, depuis le dépôt principal.
/// Refusé si le dépôt principal n'est pas propre ou pas sur la base.
pub fn merge(dir: &str) -> Result<Value> {
    let branch = ok(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let base = base_branch(dir);
    if branch == base {
        anyhow::bail!("The branch cannot be merged because this conversation already works on `{base}`.");
    }
    if !git(dir, &["status", "--porcelain"])?.trim().is_empty() {
        anyhow::bail!("The branch cannot be merged because it has uncommitted changes; commit them first.");
    }
    let root = main_root(dir).ok_or_else(|| anyhow::anyhow!("The branch cannot be merged because the main repository is unknown."))?;
    let root_branch = ok(&root, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    if root_branch != base {
        anyhow::bail!("The branch cannot be merged because the main repository is on `{root_branch}`, not `{base}`.");
    }
    if !git(&root, &["status", "--porcelain", "--untracked-files=no"])?.trim().is_empty() {
        anyhow::bail!("The branch cannot be merged because the main repository has uncommitted changes.");
    }
    git(&root, &["merge", "--no-ff", &branch, "-m", &format!("Merge branch '{branch}'")])
        .map_err(|e| anyhow::anyhow!("The branch cannot be merged because git refused: {e}"))?;
    Ok(json!({"merged": branch, "into": base}))
}

pub fn pull_request(dir: &str, draft: bool) -> Result<Value> {
    let branch = ok(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let base = base_branch(dir);
    if branch == base {
        anyhow::bail!("The pull request cannot be created because this conversation works directly on `{base}`.");
    }
    git(dir, &["push", "-u", "origin", &branch]).map_err(|e| anyhow::anyhow!("The branch cannot be pushed: {e}"))?;
    let mut args = vec!["pr", "create", "--fill", "--base", base.as_str(), "--head", branch.as_str()];
    if draft {
        args.push("--draft");
    }
    let out = Command::new("gh").current_dir(dir).args(&args).output()?;
    if !out.status.success() {
        anyhow::bail!("The pull request cannot be created because gh said: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(json!({"url": String::from_utf8_lossy(&out.stdout).trim()}))
}

/// Résumé du diff envoyé à l'IA pour proposer un message de commit (borné).
pub fn diff_for_message(dir: &str) -> Result<String> {
    let stat = git(dir, &["diff", "HEAD", "--stat"]).unwrap_or_default();
    let mut diff = git(dir, &["diff", "HEAD"]).unwrap_or_default();
    let untracked = git(dir, &["ls-files", "--others", "--exclude-standard"]).unwrap_or_default();
    diff.truncate(diff.char_indices().nth(12_000).map_or(diff.len(), |(i, _)| i));
    Ok(format!("Fichiers nouveaux :\n{untracked}\nRésumé :\n{stat}\nDiff (tronqué) :\n{diff}"))
}
