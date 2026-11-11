// Une commande `!` dans le fil : la commande, sa sortie (au fil de l'eau tant qu'elle tourne),
// ses erreurs, son code, sa durée, et le bouton qui l'arrête.
import { esc } from "./text.js";

/** Ce qui suit la commande : arrêt, ou code (s'il n'est pas 0) et durée. */
export function bashMeta(ev) {
  if (ev.stopped) return "arrêtée";
  if (ev.timed_out) return "arrêtée : délai dépassé";
  const d = ev.ms < 1000 ? `${ev.ms} ms` : `${(ev.ms / 1000).toFixed(1)} s`;
  return ev.code === 0 ? d : `code ${ev.code ?? "?"} · ${d}`;
}

/** La ligne entière ; `running` : commande en cours (roue, bouton « Arrêter », sorties vides). */
export function bashRow(ev, running = false) {
  const meta = running ? "en cours" : bashMeta(ev);
  const out = running ? "" : ev.stdout || "", err = running ? "" : ev.stderr || "";
  return `<div class="bash-run${running ? " running" : ""}" data-bash="${esc(ev.id || "")}"${running ? "" : ` title="Jointe au prochain message envoyé à Claude"`}>`
    + `<div class="bash-cmd"><b>!</b><span>${esc(ev.command)}</span><span class="bash-meta">${esc(meta)}</span>`
    + `${running ? `<button class="bash-stop" type="button" title="Arrêter la commande">Arrêter</button>` : ""}</div>`
    + `<pre class="out"${out ? "" : " hidden"}>${esc(out)}</pre><pre class="err"${err ? "" : " hidden"}>${esc(err)}</pre>`
    + `${running ? `<form class="bash-in"><input type="text" autocomplete="off" spellcheck="false" placeholder="Entrée pour la commande (Entrée pour envoyer)"></form>` : ""}</div>`;
}

/** La sortie demande-t-elle un mot de passe ? (le champ d'entrée se masque alors). */
export function asksPassword(text) {
  return /(password|passphrase|mot de passe)[^\n]*:\s*$/i.test(text);
}

/** Ajoute un morceau de sortie à `pre` en ne gardant que la fin (le DOM ne grossit pas sans fin). */
export function appendTail(prev, text, max = 200_000) {
  const s = prev + text;
  return s.length > max ? "…" + s.slice(s.length - max) : s;
}
