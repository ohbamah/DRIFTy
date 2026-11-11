//! Commande `!` (mode bash du terminal) : lancée par le shell de l'utilisateur dans le dossier
//! de la discussion, sa sortie est rendue AU FIL DE L'EAU (`on_chunk`) et elle s'arrête sur
//! demande (`stop`) ou au bout de `timeout`. Le groupe de processus entier est arrêté : une
//! commande en tuyau (`make | tee`) ne laisse pas d'orphelin.
//!
//! Entrée : ce que l'utilisateur tape (`input`) est écrit sur l'entrée standard de la commande,
//! une ligne à la fois. `sudo` y lit son mot de passe (`-S`) au lieu d'exiger un terminal :
//! une fonction shell `sudo` l'y redirige ; le mot de passe ne passe que par ce tuyau.
//! Une saisie SECRÈTE (champ mot de passe) est en plus masquée dans la sortie : une commande
//! qui réaffiche ce qu'elle lit (`read p; echo $p`) ne la montre ni à l'écran ni à Claude.

use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

/// Placée devant la commande : `sudo` lit son mot de passe sur l'entrée standard.
const PRELUDE: &str = "sudo() { command sudo -S \"$@\"; }\n";

/// Sortie gardée par flux : la FIN (`…` devant s'il a fallu couper).
pub const MAX_CHARS: usize = 30_000;

#[derive(Debug, Default)]
pub struct Outcome {
    pub stdout: String,
    pub stderr: String,
    pub code: Option<i32>,
    pub stopped: bool,
    pub timed_out: bool,
}

/// Une ligne tapée pour la commande ; `secret` : venue du champ mot de passe.
#[derive(Debug, Clone)]
pub struct Input {
    pub line: String,
    pub secret: bool,
}

/// Ce qui remplace une saisie secrète dans la sortie.
pub const MASK: &str = "••••••";

/// `text` sans aucune des saisies secrètes (les très courtes, moins de 3 caractères, ne sont pas
/// cherchées : elles effaceraient des lettres ordinaires de la sortie).
pub fn mask(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for s in secrets.iter().filter(|s| s.chars().count() >= 3) {
        if out.contains(s.as_str()) {
            out = out.replace(s.as_str(), MASK);
        }
    }
    out
}

/// Flux d'un morceau de sortie.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stream {
    Out,
    Err,
}

/// Les `n` derniers caractères de `s` (précédés de « … » s'il a fallu couper).
pub fn tail_chars(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        return s.to_string();
    }
    format!("…{}", s.chars().skip(count - n).collect::<String>())
}

/// Ajoute `text` à `buf` en ne gardant (à peu près) que la fin : la coupe exacte se fait à la fin.
fn push_tail(buf: &mut String, text: &str) {
    buf.push_str(text);
    if buf.len() > MAX_CHARS * 8 {
        *buf = tail_chars(buf, MAX_CHARS);
    }
}

/// Arrête tout le groupe de processus (la commande a son propre groupe, voir `run`).
fn kill_group(pid: Option<u32>) {
    if let Some(pid) = pid {
        unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
    }
}

pub async fn run(
    shell: &str,
    command: &str,
    cwd: &str,
    timeout: Duration,
    mut stop: oneshot::Receiver<()>,
    mut input: mpsc::Receiver<Input>,
    mut on_chunk: impl FnMut(Stream, &str),
) -> Outcome {
    let mut out = Outcome::default();
    let spawned = Command::new(shell)
        .arg("-lc")
        .arg(format!("{PRELUDE}{command}"))
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            out.stderr = format!("{shell}: {e}");
            return out;
        }
    };
    let pid = child.id();
    let (mut so, mut se) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let mut sin = child.stdin.take();
    let mut input_open = true;
    let mut secrets: Vec<String> = Vec::new();
    let (mut bo, mut be) = ([0u8; 4096], [0u8; 4096]);
    let (mut open_o, mut open_e) = (true, true);
    // Octets d'un caractère UTF-8 coupé entre deux lectures : gardés pour la suivante.
    let (mut rest_o, mut rest_e) = (Vec::new(), Vec::new());
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let decode = |rest: &mut Vec<u8>, bytes: &[u8]| -> String {
        rest.extend_from_slice(bytes);
        match std::str::from_utf8(rest) {
            Ok(s) => {
                let s = s.to_string();
                rest.clear();
                s
            }
            Err(e) if e.error_len().is_none() => {
                // Fin coupée au milieu d'un caractère : on garde la fin pour la prochaine lecture.
                let ok = e.valid_up_to();
                let s = String::from_utf8_lossy(&rest[..ok]).into_owned();
                rest.drain(..ok);
                s
            }
            Err(_) => {
                let s = String::from_utf8_lossy(rest).into_owned();
                rest.clear();
                s
            }
        }
    };
    while open_o || open_e {
        tokio::select! {
            r = so.read(&mut bo), if open_o => match r {
                Ok(0) | Err(_) => open_o = false,
                Ok(n) => { let t = mask(&decode(&mut rest_o, &bo[..n]), &secrets); if !t.is_empty() { on_chunk(Stream::Out, &t); push_tail(&mut out.stdout, &t); } }
            },
            r = se.read(&mut be), if open_e => match r {
                Ok(0) | Err(_) => open_e = false,
                Ok(n) => { let t = mask(&decode(&mut rest_e, &be[..n]), &secrets); if !t.is_empty() { on_chunk(Stream::Err, &t); push_tail(&mut out.stderr, &t); } }
            },
            line = input.recv(), if input_open => match (line, sin.as_mut()) {
                (Some(i), Some(w)) => {
                    if i.secret { secrets.push(i.line.clone()); }
                    if w.write_all(format!("{}\n", i.line).as_bytes()).await.is_err() || w.flush().await.is_err() { sin = None; }
                }
                (None, _) => input_open = false,
                _ => {}
            },
            _ = &mut stop => { out.stopped = true; kill_group(pid); break; }
            _ = &mut deadline => { out.timed_out = true; kill_group(pid); break; }
        }
    }
    if !out.stopped && !out.timed_out {
        out.code = child.wait().await.ok().and_then(|s| s.code());
    } else {
        let _ = child.wait().await;
    }
    // Masquage final sur la sortie entière : rattrape une saisie coupée entre deux lectures.
    out.stdout = tail_chars(mask(&out.stdout, &secrets).trim_end(), MAX_CHARS);
    out.stderr = tail_chars(mask(&out.stderr, &secrets).trim_end(), MAX_CHARS);
    out
}

/// Ce que Claude lit de la commande, au format du terminal (avec l'arrêt s'il y en a eu un).
pub fn context_block(command: &str, o: &Outcome, timeout: Duration) -> String {
    let note = if o.stopped {
        "\n[Command stopped by the user.]".to_string()
    } else if o.timed_out {
        format!("\n[Command stopped after {} s.]", timeout.as_secs())
    } else {
        String::new()
    };
    format!("<bash-input>{command}</bash-input>\n<bash-stdout>{}</bash-stdout><bash-stderr>{}{note}</bash-stderr>", o.stdout, o.stderr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_the_end() {
        assert_eq!(tail_chars("abc", 5), "abc");
        assert_eq!(tail_chars("abcdéf", 3), "…déf");
    }

    #[tokio::test]
    async fn streams_both_outputs_and_exit_code() {
        let (_tx, rx) = oneshot::channel();
        let mut chunks = vec![];
        let o = run("/bin/sh", "echo un; echo deux >&2; exit 3", "/", Duration::from_secs(10), rx, mpsc::channel(1).1, |s, t| chunks.push((s, t.to_string()))).await;
        assert_eq!((o.stdout.as_str(), o.stderr.as_str(), o.code), ("un", "deux", Some(3)));
        assert!(chunks.contains(&(Stream::Out, "un\n".into())) && chunks.contains(&(Stream::Err, "deux\n".into())));
        assert!(!o.stopped && !o.timed_out);
    }

    #[tokio::test]
    async fn output_arrives_before_the_end() {
        // La 1re ligne arrive pendant que la commande dort encore : rendu au fil de l'eau.
        let (_tx, rx) = oneshot::channel();
        let start = std::time::Instant::now();
        let mut first = None;
        run("/bin/sh", "echo tot; sleep 1; echo tard", "/", Duration::from_secs(10), rx, mpsc::channel(1).1, |_, _| {
            first.get_or_insert(start.elapsed());
        })
        .await;
        assert!(first.unwrap() < Duration::from_millis(800), "{first:?}");
    }

    #[tokio::test]
    async fn stop_kills_the_whole_pipeline() {
        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let _ = tx.send(());
        });
        let start = std::time::Instant::now();
        let o = run("/bin/sh", "echo go; sleep 30 | cat", "/", Duration::from_secs(60), rx, mpsc::channel(1).1, |_, _| {}).await;
        assert!(o.stopped && o.code.is_none() && o.stdout == "go");
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(context_block("x", &o, Duration::from_secs(60)).contains("stopped by the user"));
    }

    #[tokio::test]
    async fn timeout_stops_it() {
        let (_tx, rx) = oneshot::channel();
        let o = run("/bin/sh", "sleep 30", "/", Duration::from_millis(300), rx, mpsc::channel(1).1, |_, _| {}).await;
        assert!(o.timed_out && o.code.is_none());
    }

    #[tokio::test]
    async fn input_reaches_the_command() {
        let (_tx, rx) = oneshot::channel();
        let (itx, irx) = mpsc::channel(4);
        let feed = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            itx.send(Input { line: "secret".into(), secret: false }).await.unwrap();
            itx // gardé ouvert : la commande attend vraiment sa ligne
        });
        let o = run("/bin/sh", "printf 'Password:' >&2; read p; echo \"lu=$p\"", "/", Duration::from_secs(10), rx, irx, |_, _| {}).await;
        let _ = feed.await;
        assert_eq!((o.stdout.as_str(), o.stderr.as_str()), ("lu=secret", "Password:"));
    }

    // À la demande (`cargo test -- --ignored`) : vide le cache de sudo et laisse des tentatives
    // de mot de passe ratées dans les journaux du système.
    #[tokio::test]
    #[ignore]
    async fn sudo_reads_its_password_from_input() {
        // `sudo -k -S` : sans mot de passe en cache ; un faux mot de passe est refusé par sudo
        // lui-même (et non « a terminal is required ») : la saisie lui parvient bien.
        let (_tx, rx) = oneshot::channel();
        let (itx, irx) = mpsc::channel(4);
        let feed = tokio::spawn(async move {
            for _ in 0..3 {
                tokio::time::sleep(Duration::from_millis(300)).await;
                let _ = itx.send(Input { line: "pas-le-bon".into(), secret: true }).await;
            }
        });
        let o = run("/bin/sh", "sudo -k; sudo true", "/", Duration::from_secs(20), rx, irx, |_, _| {}).await;
        let _ = feed.await;
        assert!(!o.stderr.contains("terminal is required"), "{}", o.stderr);
        assert!(o.stderr.to_lowercase().contains("password"), "{}", o.stderr);
    }

    #[tokio::test]
    async fn a_secret_echoed_by_the_command_is_masked() {
        // Le script réaffiche ce qu'il lit, en deux temps (coupé entre deux lectures) et sur
        // les deux flux : ni l'écran (morceaux) ni Claude (contexte) ne voient le mot de passe.
        let (_tx, rx) = oneshot::channel();
        let (itx, irx) = mpsc::channel(4);
        let feed = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            itx.send(Input { line: "Tr0ub4dor".into(), secret: true }).await.unwrap();
            itx
        });
        let mut shown = String::new();
        let o = run("/bin/sh", "printf 'Password:' >&2; read p; echo \"lu=$p\"; echo \"$p\" >&2", "/", Duration::from_secs(10), rx, irx, |_, t| shown.push_str(t)).await;
        let _ = feed.await;
        assert_eq!(o.stdout, format!("lu={MASK}"));
        assert_eq!(o.stderr, format!("Password:{MASK}"));
        assert!(!shown.contains("Tr0ub4dor") && !context_block("x", &o, Duration::from_secs(1)).contains("Tr0ub4dor"));
        // Une saisie ordinaire, elle, reste visible.
        assert_eq!(mask("oui", &["abc".into()]), "oui");
        assert_eq!(mask("y", &["y".into()]), "y");
    }

    #[tokio::test]
    async fn utf8_split_across_reads_is_kept() {
        let (_tx, rx) = oneshot::channel();
        let o = run("/bin/sh", "printf 'd\\303'; sleep 0.2; printf '\\251j\\303\\240'", "/", Duration::from_secs(10), rx, mpsc::channel(1).1, |_, _| {}).await;
        assert_eq!(o.stdout, "déjà");
    }
}
