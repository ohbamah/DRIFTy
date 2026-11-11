//! Ingestion incrémentale des transcripts des comptes configurés.
//! Un fichier n'est relu qu'à partir de l'octet où l'on s'était arrêté.

use crate::db::{self, Ingested};
use crate::extract::FileBatch;
use anyhow::Result;
use rayon::prelude::*;
use rusqlite::Connection;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Report {
    pub files_seen: usize,
    pub files_read: usize,
    pub lines: u64,
    pub bad_lines: u64,
    pub messages: usize,
    pub tool_calls: usize,
    pub millis: u128,
}

struct Candidate {
    path: PathBuf,
    slot: String,
    project_dir: String,
    size: u64,
    mtime: i64,
    from: u64,
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if p.file_name().is_some_and(|n| n == "tool-results") {
                continue;
            }
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "jsonl") {
            out.push(p);
        }
    }
}

/// Lit `path` à partir de `from` ; s'arrête à la dernière ligne COMPLÈTE
/// (une ligne en cours d'écriture sera relue au prochain passage).
fn read_from(path: &Path, from: u64) -> Result<(FileBatch, u64)> {
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(from))?;
    let mut r = BufReader::with_capacity(1 << 20, f);
    let mut batch = FileBatch::default();
    let mut offset = from;
    let mut buf = Vec::with_capacity(1 << 16);
    loop {
        buf.clear();
        let n = r.read_until(b'\n', &mut buf)?;
        if n == 0 || buf.last() != Some(&b'\n') {
            break;
        }
        offset += n as u64;
        match std::str::from_utf8(&buf) {
            Ok(s) => batch.push_line(s),
            Err(_) => batch.bad_lines += 1,
        }
    }
    Ok((batch, offset))
}

/// Nom du dossier de transcripts que Claude Code donne au dossier de travail `path` :
/// tout signe autre qu'une lettre ou un chiffre ASCII devient `-`
/// (`/Users/x/drifty/worktrees/archc/S-2` → `-Users-x-drifty-worktrees-archc-S-2`).
pub fn project_dir_name(path: &str) -> String {
    path.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// Le dossier de transcripts `dir` est-il celui d'un des dossiers `only`, ou d'un de leurs
/// sous-dossiers (une session lancée plus bas dans le worktree) ?
fn in_scope(dir: &str, only: &[String]) -> bool {
    only.iter().any(|o| dir == o || dir.strip_prefix(o.as_str()).is_some_and(|r| r.starts_with('-')))
}

/// Toute l'ingestion (bouton « Analyser »).
pub fn run(conn: &mut Connection) -> Result<Report> {
    run_in(conn, None)
}

/// Ingestion limitée aux transcripts des dossiers de travail `only` (noms de `project_dir_name`),
/// ou complète si `None`. Même chemin dans les deux cas : seul le choix des fichiers change.
pub fn run_in(conn: &mut Connection, only: Option<&[String]>) -> Result<Report> {
    let start = Instant::now();
    let accounts = db::accounts(conn)?;
    let known = db::file_states(conn)?;
    let mut cands = Vec::new();
    let mut seen = 0;
    for acc in accounts.iter().filter(|a| a.enabled) {
        let projects = Path::new(&acc.config_dir).join("projects");
        let mut paths = Vec::new();
        walk(&projects, &mut paths);
        for p in paths {
            seen += 1;
            let Ok(md) = p.metadata() else { continue };
            let size = md.len();
            let mtime = md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64);
            let key = p.to_string_lossy().into_owned();
            let from = match known.get(&key) {
                Some(k) if k.size == size && k.mtime == mtime => continue,
                // Fichier réécrit plus court : on repart de zéro (les insertions sont idempotentes).
                Some(k) if size < k.offset => 0,
                Some(k) => k.offset,
                None => 0,
            };
            let project_dir = p
                .strip_prefix(&projects)
                .ok()
                .and_then(|r| r.components().next())
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .unwrap_or_default();
            if only.is_some_and(|o| !in_scope(&project_dir, o)) {
                continue;
            }
            cands.push(Candidate { path: p, slot: acc.slot.clone(), project_dir, size, mtime, from });
        }
    }

    let files: Vec<Ingested> = cands
        .into_par_iter()
        .filter_map(|c| {
            let (batch, offset) = read_from(&c.path, c.from).ok()?;
            Some(Ingested {
                path: c.path.to_string_lossy().into_owned(),
                slot: c.slot,
                project_dir: c.project_dir,
                size: c.size,
                mtime: c.mtime,
                offset,
                is_main: !c.path.components().any(|x| x.as_os_str() == "subagents"),
                batch,
            })
        })
        .collect();

    let mut rep = Report { files_seen: seen, files_read: files.len(), ..Default::default() };
    for f in &files {
        rep.lines += f.batch.lines;
        rep.bad_lines += f.batch.bad_lines;
        rep.messages += f.batch.messages.len();
        rep.tool_calls += f.batch.tool_uses.len();
    }
    db::write(conn, &files)?;
    rep.millis = start.elapsed().as_millis();
    Ok(rep)
}

#[cfg(test)]
mod scope_tests {
    #[test]
    fn a_worktree_and_its_subfolders_are_in_scope() {
        let wt = super::project_dir_name("/Users/x/drifty/worktrees/archc/Silencieux-2-decisions");
        assert_eq!(wt, "-Users-x-drifty-worktrees-archc-Silencieux-2-decisions");
        let only = [wt.clone()];
        assert!(super::in_scope(&wt, &only));
        assert!(super::in_scope(&format!("{wt}-tests"), &only)); // sous-dossier `tests`
        assert!(!super::in_scope("-Users-x-drifty-worktrees-archc-Silencieux-2", &only));
        assert!(!super::in_scope("-Users-x-dev-archc", &only));
        assert_eq!(super::project_dir_name("/a/b_c.d é"), "-a-b-c-d--");
    }
}
