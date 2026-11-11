//! Les discussions en direct survivent à un redémarrage du démon (mise à jour de l'app,
//! plantage) : leur description est tenue dans un fichier à côté de la base, et chacune est
//! relancée au démarrage en REPRISE de sa session, sous le même identifiant (liens et barre
//! latérale restent valables). Relancer ne coûte rien : rien n'est envoyé à Claude.
//! Une discussion fermée exprès (« Fermer ») sort du fichier.

use crate::chat::{Hub, LaunchSpec};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Ce qu'il faut pour relancer une discussion.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Saved {
    pub id: String,
    pub repo_id: i64,
    pub name: String,
    pub cwd: String,
    pub slot: Option<String>,
    pub config_dir: Option<String>,
    pub bypass: bool,
    pub worktree: Option<String>,
    pub model: Option<String>,
    pub session_id: String,
    #[serde(default)]
    pub env: Vec<(String, String)>,
    #[serde(default)]
    pub tokens: i64,
    #[serde(default)]
    pub cost_usd: f64,
    /// Claude travaillait quand le démon s'est arrêté : son tour a été coupé.
    #[serde(default)]
    pub interrupted: bool,
}

/// `drifty.db` → `drifty.chats.json` (une base de test a donc son propre fichier).
pub fn file() -> PathBuf {
    drifty_core::db_path().with_extension("chats.json")
}

fn read() -> Vec<Saved> {
    std::fs::read_to_string(file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn write(list: &[Saved]) -> std::io::Result<()> {
    let path = file();
    let tmp = path.with_extension("chats.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(list).unwrap_or_default())?;
    std::fs::rename(tmp, path) // remplacement atomique : jamais de fichier à moitié écrit
}

/// Relance chaque discussion du fichier ; rend le nombre rétablies. À appeler AVANT `keep`.
pub async fn restore(hub: &Hub) -> usize {
    let mut n = 0;
    for s in read() {
        if !std::path::Path::new(&s.cwd).is_dir() {
            eprintln!("discussion {} non rétablie : le dossier {} n'existe plus", s.id, s.cwd);
            continue;
        }
        let spec = LaunchSpec {
            repo_id: s.repo_id,
            name: s.name.clone(),
            cwd: s.cwd.clone(),
            slot: s.slot.clone(),
            config_dir: s.config_dir.clone(),
            resume: Some(s.session_id.clone()),
            bypass: s.bypass,
            worktree: s.worktree.clone(),
            tokens_base: s.tokens,
            env: s.env.clone(),
            new_worktree: false,
            setup_job: None,
            cost_base: s.cost_usd,
            model: s.model.clone(),
            fork: false,
            restore_id: Some(s.id.clone()),
        };
        match hub.start(spec, None).await {
            Ok(chat) => {
                n += 1;
                if s.interrupted {
                    chat.notice("DRIFTy a redémarré pendant que Claude travaillait : son dernier tour a été coupé. Écris « continue » pour qu'il reprenne.");
                }
            }
            Err(e) => eprintln!("discussion {} non rétablie : {e:#}", s.id),
        }
    }
    n
}

/// Tient le fichier à jour : réécrit (si besoin) peu après chaque changement de la liste.
pub fn keep(hub: Hub) {
    tokio::spawn(async move {
        let mut rx = hub.watch();
        let mut last = read();
        loop {
            if let Err(tokio::sync::broadcast::error::RecvError::Closed) = rx.recv().await {
                break;
            }
            // Regroupe une rafale de changements (un tour de Claude en produit beaucoup).
            tokio::time::sleep(Duration::from_millis(300)).await;
            while rx.try_recv().is_ok() {}
            let now = hub.saved();
            if now != last {
                if let Err(e) = write(&now) {
                    eprintln!("discussions non enregistrées : {e}");
                }
                last = now;
            }
        }
    });
}

/// Arrêt demandé (SIGTERM de l'app qui quitte, Ctrl-C) : les discussions sont enregistrées
/// PUIS leurs processus arrêtés par leur PID. Sans ça, le démon mourait sur-le-champ et ses
/// `claude` continuaient seuls, en orphelins, sur des sessions qu'on relance ensuite.
pub fn on_shutdown(hub: Hub) {
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let (Ok(mut term), Ok(mut int)) = (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) else { return };
        tokio::select! { _ = term.recv() => {}, _ = int.recv() => {} }
        // Avant l'arrêt : un tour en cours est encore « working », donc noté comme coupé.
        let _ = write(&hub.saved());
        hub.stop_all().await;
        std::process::exit(0);
    });
}

#[cfg(test)]
mod tests {
    use super::Saved;

    #[test]
    fn the_capture_written_by_app_sh_is_read_back() {
        // Format écrit par `app.sh --install` depuis `/api/chats` (passage depuis une ancienne version).
        let s = r#"[{"id": "c1", "repo_id": 7, "name": "Contextator", "cwd": "/tmp", "slot": "perso",
            "config_dir": "/Users/x/.claude", "bypass": true, "worktree": null, "model": "claude-opus-5-5",
            "session_id": "80b7a339", "env": [], "tokens": 12, "cost_usd": 174.78, "interrupted": false}]"#;
        let v: Vec<Saved> = serde_json::from_str(s).unwrap();
        assert_eq!((v[0].id.as_str(), v[0].session_id.as_str(), v[0].cost_usd), ("c1", "80b7a339", 174.78));
        // Et le format du démon (avec variables) fait l'aller-retour.
        let back: Vec<Saved> = serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(back, v);
    }
}
