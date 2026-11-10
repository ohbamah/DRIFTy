//! `claudiusd statusline --slot <compte> [-- <commande d'origine>]`
//!
//! Appelé par Claude Code à chaque mise à jour de sa statusline : relève
//! `rate_limits` pour DRIFTy, puis rejoue la statusline d'origine avec la
//! même entrée et rend sa sortie telle quelle. Ne doit jamais casser la
//! statusline : une erreur de relevé va sur stderr, jamais sur stdout.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

pub fn run(args: &[String]) -> i32 {
    let slot = args.iter().position(|a| a == "--slot").and_then(|i| args.get(i + 1)).cloned().unwrap_or_default();
    let wrapped = args.iter().position(|a| a == "--").map(|i| args[i + 1..].join(" "));

    let mut input = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut input);
    if let Err(e) = claudius_core::usage::record(&slot, &input) {
        eprintln!("claudius: relevé d'usage impossible : {e:#}");
    }

    let Some(cmd) = wrapped.filter(|c| !c.trim().is_empty()) else { return 0 };
    let child = Command::new("/bin/sh").arg("-c").arg(&cmd).stdin(Stdio::piped()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            eprintln!("claudius: la statusline d'origine ne peut pas démarrer : {e}");
            return 1;
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&input);
    }
    child.wait().map_or(1, |s| s.code().unwrap_or(1))
}
