// Étoile « améliorer le prompt » en haut à droite d'une zone de saisie.
// Clic : Claude propose une version améliorée ; on garde la sienne ou on prend la proposition.
import { api, esc, tok, usd, icon, toast } from "./lib.js";

/** `ctx()` rend le contexte de l'appel : { repo_id } | { chat_id } | { session_id }. */
export function attachImprove(textarea, ctx) {
  const wrap = document.createElement("div");
  wrap.className = "ta-wrap";
  textarea.replaceWith(wrap);
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "improve-btn";
  btn.title = "Améliorer le prompt";
  btn.setAttribute("aria-label", "Améliorer le prompt");
  btn.innerHTML = icon("sparkle");
  const panel = document.createElement("div");
  panel.className = "improve";
  panel.hidden = true;
  wrap.append(panel, textarea, btn);

  const close = () => { panel.hidden = true; panel.innerHTML = ""; };
  btn.addEventListener("click", async () => {
    const text = textarea.value.trim();
    if (!text) return textarea.focus();
    btn.classList.add("busy");
    btn.disabled = true;
    try {
      const r = await api("/api/improve", { method: "POST", json: { text, ...ctx() } });
      panel.innerHTML = `
        <div class="improve-head"><span>${icon("sparkle")}Prompt amélioré</span><span class="muted">${tok(r.input_tokens + r.output_tokens)} tokens, ${usd(r.cost_usd)}</span></div>
        <div class="improve-text">${esc(r.text)}</div>
        <div class="improve-actions"><button class="btn" type="button" data-keep>Garder le mien</button><button class="btn primary" type="button" data-use>Utiliser</button></div>`;
      panel.hidden = false;
      panel.querySelector("[data-keep]").addEventListener("click", () => { close(); textarea.focus(); });
      panel.querySelector("[data-use]").addEventListener("click", () => {
        textarea.value = r.text;
        textarea.dispatchEvent(new Event("input", { bubbles: true })); // brouillon et hauteur suivent
        close();
        textarea.focus();
      });
    } catch (e) { toast(e.message, true); }
    btn.classList.remove("busy");
    btn.disabled = false;
  });
  return wrap;
}
