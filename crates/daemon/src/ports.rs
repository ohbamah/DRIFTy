//! Un port par worktree (comme Conductor) : une plage de 10 ports à partir de 4000,
//! attribuée une fois et gardée. Fournie en `PORT` et `DRIFTY_PORT` aux commandes
//! et aux agents du worktree. Le dépôt principal garde le port habituel de l'app.

use anyhow::Result;
use rusqlite::{params, Connection};

const FIRST: u16 = 4000;
const STEP: u16 = 10;

fn free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Port du worktree `worktree` du projet (attribué au besoin) ; `None` pour le dépôt principal.
pub fn for_worktree(conn: &Connection, repo_id: i64, worktree: &str) -> Result<Option<u16>> {
    if worktree.is_empty() {
        return Ok(None);
    }
    let key = format!("port:{repo_id}:{worktree}");
    if let Some(p) = drifty_core::db::setting(conn, &key)?.and_then(|v| v.parse::<u16>().ok()) {
        return Ok(Some(p));
    }
    let mut st = conn.prepare("SELECT v FROM settings WHERE k LIKE 'port:%'")?;
    let taken: Vec<u16> = st.query_map([], |r| r.get::<_, String>(0))?.flatten().filter_map(|v| v.parse().ok()).collect();
    let mut p = FIRST;
    while taken.contains(&p) || !free(p) {
        p = p.checked_add(STEP).ok_or_else(|| anyhow::anyhow!("The worktree cannot get a port because none is free."))?;
    }
    conn.execute("INSERT OR REPLACE INTO settings(k, v) VALUES (?1, ?2)", params![key, p.to_string()])?;
    Ok(Some(p))
}

/// Variables d'environnement d'un worktree (vide pour le dépôt principal).
pub fn env(port: Option<u16>) -> Vec<(String, String)> {
    port.map(|p| vec![("PORT".to_string(), p.to_string()), ("DRIFTY_PORT".to_string(), p.to_string())]).unwrap_or_default()
}

/// Port fixé en dur dans un Procfile (qui ignorerait alors `PORT`) : `-p 3000`, `--port 3000`, `PORT=3000`.
pub fn hardcoded(dir: &str) -> Option<String> {
    for f in ["Procfile.dev", "Procfile"] {
        let Ok(text) = std::fs::read_to_string(std::path::Path::new(dir).join(f)) else { continue };
        for line in text.lines().filter(|l| l.starts_with("web")) {
            let words: Vec<&str> = line.split_whitespace().collect();
            for w in words.windows(2) {
                if (w[0] == "-p" || w[0] == "--port") && w[1].chars().all(|c| c.is_ascii_digit()) {
                    return Some(format!("{f} : « {} »", line.trim()));
                }
            }
            if words.iter().any(|w| w.starts_with("PORT=") && w[5..].chars().all(|c| c.is_ascii_digit())) {
                return Some(format!("{f} : « {} »", line.trim()));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn hardcoded_port_is_spotted() {
        let d = std::env::temp_dir().join(format!("drifty-ports-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("Procfile.dev"), "web: bin/rails server -p ${PORT:-3000}\n").unwrap();
        assert!(super::hardcoded(d.to_str().unwrap()).is_none());
        std::fs::write(d.join("Procfile.dev"), "web: bin/rails server -p 3000\n").unwrap();
        assert!(super::hardcoded(d.to_str().unwrap()).is_some());
        std::fs::remove_dir_all(&d).ok();
    }
}
