// BTW : question annexe (/btw de Claude Code), posée à côté de la conversation sans la
// modifier ni interrompre l'agent. Panneau au-dessus de la saisie : la réponse, l'historique
// des questions annexes (◀ ▶, ou Maj+← / Maj+→), et ce qu'on peut en faire : copier,
// insérer dans la saisie, envoyer à l'agent, ouvrir une branche (fork) qui part de là.
import { api, esc, icon, toast, $ } from "./lib.js";
import { md } from "./md.js";

const KEY = (t) => "drifty.btw." + new URLSearchParams(t).toString();
const loadHist = (t) => { try { return JSON.parse(localStorage.getItem(KEY(t)) || "[]"); } catch { return []; } };
const saveHist = (t, h) => { try { localStorage.setItem(KEY(t), JSON.stringify(h.slice(-20))); } catch {} };

/**
 * opts : target() → `{chat}` | `{session}` ; insert(text) ; send(text) (agent en direct, sinon absent) ;
 * fork(question, answer) → ouvre une branche de la conversation.
 */
export function openBtw(composerEl, opts) {
  composerEl.querySelector(".btw-panel")?.remove();
  const target = opts.target();
  let hist = loadHist(target), pos = hist.length - 1, busy = false;
  const panel = document.createElement("div");
  panel.className = "btw-panel";
  composerEl.append(panel);
  const close = () => { panel.remove(); removeEventListener("keydown", keys, true); };

  const draw = () => {
    const cur = hist[pos];
    panel.innerHTML = `
      <div class="btw-head"><span class="btw-tag">BTW</span><span class="muted small">Question annexe : n'entre pas dans la conversation et n'interrompt pas l'agent.</span><span style="flex:1"></span>
        ${hist.length > 1 ? `<button class="icon-btn" data-prev title="Question précédente (Maj+←)"${pos <= 0 ? " disabled" : ""}>${icon("back")}</button><span class="muted small">${pos + 1}/${hist.length}</span><button class="icon-btn" data-next title="Question suivante (Maj+→)"${pos >= hist.length - 1 ? " disabled" : ""}>${icon("chevron")}</button>` : ""}
        <button class="icon-btn" data-close title="Fermer (Échap)">${icon("close")}</button></div>
      ${cur ? `<div class="btw-q">${esc(cur.question)}</div>
        <div class="btw-a md">${cur.pending ? `<span class="spin" style="width:14px;height:14px;border-width:2px;display:inline-block"></span> <span class="muted">Claude réfléchit…</span>` : cur.error ? `<span class="err">${esc(cur.error)}</span>` : md(cur.response)}</div>
        ${cur.response && !cur.pending ? `<div class="btw-actions">
          <button class="btn small" data-copy>${icon("copy")}Copier</button>
          <button class="btn small" data-insert>Insérer dans la saisie</button>
          ${opts.send ? `<button class="btn small" data-send>Envoyer à l'agent</button>` : ""}
          <button class="btn small" data-fork title="Nouvelle conversation qui part de celle-ci, avec cette question et sa réponse">${icon("branch")}Fork</button>
        </div>` : ""}` : ""}
      <div class="btw-ask"><textarea class="field" rows="1" placeholder="Pose une question annexe…"></textarea><button class="btn primary small" data-ask>Demander</button></div>`;
    const ta = $("textarea", panel);
    const ask = async () => {
      const q = ta.value.trim();
      if (!q || busy) return;
      busy = true;
      const history = hist.filter((h) => h.response && !h.pending).map((h) => ({ question: h.question, response: h.response }));
      hist.push({ question: q, pending: true });
      pos = hist.length - 1;
      draw();
      try {
        const r = await api("/api/btw", { method: "POST", json: { ...target, question: q, history } });
        hist[hist.length - 1] = { question: q, response: r.response, at: Date.now() };
      } catch (e) {
        hist[hist.length - 1] = { question: q, error: e.message };
      }
      busy = false;
      saveHist(target, hist.filter((h) => h.response));
      if (panel.isConnected) draw();
    };
    ta.addEventListener("keydown", (e) => { if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); ask(); } });
    $("[data-ask]", panel).addEventListener("click", ask);
    $("[data-close]", panel).addEventListener("click", close);
    $("[data-prev]", panel)?.addEventListener("click", () => { pos = Math.max(0, pos - 1); draw(); });
    $("[data-next]", panel)?.addEventListener("click", () => { pos = Math.min(hist.length - 1, pos + 1); draw(); });
    const a = hist[pos];
    $("[data-copy]", panel)?.addEventListener("click", async () => { await navigator.clipboard.writeText(a.response).catch(() => {}); toast("Réponse copiée."); });
    $("[data-insert]", panel)?.addEventListener("click", () => { opts.insert(a.response); close(); });
    $("[data-send]", panel)?.addEventListener("click", () => { opts.send(`À propos de ma question annexe « ${a.question} », tu as répondu :\n\n${a.response}`); close(); });
    $("[data-fork]", panel)?.addEventListener("click", async (e) => {
      e.currentTarget.disabled = true;
      try { await opts.fork(a.question, a.response); close(); } catch (err) { toast(err.message, true); e.currentTarget.disabled = false; }
    });
    if (!busy) ta.focus();
  };
  const keys = (e) => {
    if (!panel.isConnected) return removeEventListener("keydown", keys, true);
    if (e.key === "Escape") { e.preventDefault(); close(); }
    if (e.shiftKey && (e.key === "ArrowLeft" || e.key === "ArrowRight") && hist.length > 1) {
      e.preventDefault();
      pos = Math.max(0, Math.min(hist.length - 1, pos + (e.key === "ArrowLeft" ? -1 : 1)));
      draw();
    }
  };
  addEventListener("keydown", keys, true);
  draw();
}

/** Message d'ouverture d'une branche née d'une question annexe. */
export const forkMessage = (q, a) =>
  `Je reprends depuis une question annexe que je t'ai posée.\n\nQuestion : ${q}\n\nTa réponse :\n${a}\n\nPoursuivons à partir de là.`;
