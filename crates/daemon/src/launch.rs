//! Ouvre un terminal sur un projet ou une conversation, avec le bon compte.

use anyhow::Result;
use claudius_core::repos::LaunchTarget;
use std::process::Command;

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// La commande shell exécutée dans le terminal.
pub fn command(t: &LaunchTarget) -> String {
    let default_dir = claudius_core::home().join(".claude");
    let mut cmd = format!("cd {} && ", shell_quote(&t.dir));
    // `~/.claude` est le dossier par défaut de Claude : ne pas le forcer, la
    // variable déplace aussi le fichier d'identification.
    if let Some(dir) = &t.config_dir {
        if std::path::Path::new(dir) != default_dir {
            cmd += &format!("CLAUDE_CONFIG_DIR={} ", shell_quote(dir));
        }
    }
    cmd += "claude";
    if let Some(id) = &t.resume {
        cmd += &format!(" --resume {}", shell_quote(id));
    }
    cmd
}

pub fn open_terminal(t: &LaunchTarget) -> Result<String> {
    let cmd = command(t);
    if !cfg!(target_os = "macos") {
        anyhow::bail!("The terminal cannot be opened because this is only supported on macOS for now.");
    }
    let status = Command::new("osascript")
        .arg("-e")
        .arg(format!("tell application \"Terminal\" to do script {}", applescript_string(&cmd)))
        .arg("-e")
        .arg("tell application \"Terminal\" to activate")
        .status()?;
    if !status.success() {
        anyhow::bail!("The terminal cannot be opened because osascript exited with {status}.");
    }
    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        let t = LaunchTarget { dir: "/a b/it's".into(), config_dir: Some("/x/.claude-pro".into()), resume: Some("abc".into()) };
        assert_eq!(command(&t), r#"cd '/a b/it'\''s' && CLAUDE_CONFIG_DIR='/x/.claude-pro' claude --resume 'abc'"#);
        assert_eq!(applescript_string(r#"a"b\c"#), r#""a\"b\\c""#);
    }
}
