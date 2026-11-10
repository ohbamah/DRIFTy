// Questions à choix de Claude (`AskUserQuestion`), comme dans le terminal : un onglet par
// question, des choix numérotés avec leur description, « Autre » en texte libre, cases à
// cocher pour un choix multiple, puis un récapitulatif avant l'envoi (plusieurs questions).
// Clavier : 1–9 choisit, ↑/↓ déplace, Entrée valide, ←/→ ou Tab change d'onglet, Échap annule.
import { esc } from "./lib.js";

/**
 * Monte la carte dans `card` (un élément vide) pour la demande `req` (`can_use_tool`).
 * `answer(answers)` : envoie les réponses (question → choix) ; `refuse(message)` : refuse,
 * avec le motif que Claude lira ; `chat()` : l'utilisateur préfère en parler (saisie).
 */
export function mountAsk(card, req, { answer, refuse, chat }) {
  const qs = req.input?.questions || [];
  const many = qs.length > 1;
  const picked = qs.map(() => new Set()); // choix cochés (libellés)
  const other = qs.map(() => "");         // texte libre (« Autre »)
  let tab = 0;      // question affichée ; qs.length = récapitulatif
  let cursor = 0;   // choix en surbrillance
  let busy = false;

  const value = (i) => [...picked[i], ...(other[i].trim() ? [other[i].trim()] : [])].join(", ");
  const done = (i) => !!value(i);
  const options = (i) => qs[i].options || [];

  const send = async () => {
    if (busy) return;
    busy = true;
    const answers = Object.fromEntries(qs.map((q, i) => [q.question, value(i)]));
    try { await answer(answers); } catch { busy = false; }
  };
  const cancel = () => { if (!busy) { busy = true; refuse("The user declined to answer these questions.").catch(() => { busy = false; }); } };
  const discuss = () => { if (!busy) { busy = true; refuse("The user wants to discuss these questions in the chat instead of choosing an answer. Wait for their message.").then(chat).catch(() => { busy = false; }); } };

  const go = (t) => { tab = Math.max(0, Math.min(t, many ? qs.length : 0)); cursor = 0; draw(); card.focus({ preventScroll: true }); };
  // Choix d'une option (unique : passe à la suite ; multiple : coche / décoche).
  const choose = (i, k) => {
    const q = qs[i], label = options(i)[k]?.label;
    if (label == null) return;
    if (q.multiSelect) { picked[i].has(label) ? picked[i].delete(label) : picked[i].add(label); draw(); return; }
    picked[i] = new Set([label]); other[i] = "";
    next(i);
  };
  const next = (i) => { if (!many) return send(); go(qs.findIndex((_, j) => j > i && !done(j)) >= 0 ? qs.findIndex((_, j) => j > i && !done(j)) : qs.length); };

  const tabsHtml = () => !many ? "" : `<div class="ask-tabs">${qs.map((q, i) => `<button type="button" class="ask-tab${i === tab ? " on" : ""}" data-tab="${i}"><span class="ask-box">${done(i) ? "☒" : "☐"}</span>${esc(q.header || `Question ${i + 1}`)}</button>`).join("")}<button type="button" class="ask-tab${tab === qs.length ? " on" : ""}" data-tab="${qs.length}">✓ Envoyer</button></div>`;

  const questionHtml = (i) => {
    const q = qs[i], opts = options(i), n = opts.length;
    const row = (k, inner, cls = "") => `<div class="ask-opt${k === cursor ? " cur" : ""}${cls}" data-k="${k}"><span class="ask-n">${k + 1}.</span>${inner}</div>`;
    return `<div class="ask-q">${esc(q.question)}${q.multiSelect ? ` <span class="muted">(plusieurs choix possibles)</span>` : ""}</div>
      <div class="ask-opts">
        ${opts.map((o, k) => row(k, `${q.multiSelect ? `<span class="ask-check">${picked[i].has(o.label) ? "☒" : "☐"}</span>` : ""}<div><div class="ask-label">${esc(o.label)}</div>${o.description ? `<div class="ask-desc">${esc(o.description)}</div>` : ""}</div>`, !q.multiSelect && picked[i].has(o.label) ? " sel" : "")).join("")}
        ${row(n, `<input class="ask-other" type="text" placeholder="Autre réponse…" value="${esc(other[i])}">`)}
      </div>
      <div class="ask-foot">
        ${q.multiSelect || many ? `<button type="button" class="btn primary" data-next ${done(i) ? "" : "disabled"}>${many ? "Suivant" : "Envoyer"}</button>` : ""}
        <button type="button" class="link" data-chat>En discuter avec Claude</button>
        <button type="button" class="link" data-cancel>Annuler</button>
        <span class="ask-keys">1–${n + 1} choisir · ↑↓ Entrée${many ? " · ←→ onglets" : ""} · Échap annuler</span>
      </div>`;
  };

  const reviewHtml = () => `<div class="ask-q">Vérifie tes réponses</div>
    <ul class="ask-review">${qs.map((q, i) => `<li><div>${esc(q.question)}</div><div class="${done(i) ? "ask-ans" : "ask-miss"}">→ ${done(i) ? esc(value(i)) : "sans réponse"}</div></li>`).join("")}</ul>
    <div class="ask-foot"><button type="button" class="btn primary" data-send>Envoyer les réponses</button><button type="button" class="link" data-chat>En discuter avec Claude</button><button type="button" class="link" data-cancel>Annuler</button></div>`;

  function draw() {
    const keepOther = document.activeElement?.classList.contains("ask-other");
    card.innerHTML = `${tabsHtml()}<div class="ask-body">${tab < qs.length ? questionHtml(tab) : reviewHtml()}</div>`;
    if (keepOther) { const inp = card.querySelector(".ask-other"); inp?.focus(); inp?.setSelectionRange(inp.value.length, inp.value.length); }
  }

  card.tabIndex = 0;
  card.addEventListener("click", (e) => {
    const t = e.target;
    const tb = t.closest("[data-tab]"); if (tb) return go(+tb.dataset.tab);
    if (t.closest("[data-send]")) return send();
    if (t.closest("[data-cancel]")) return cancel();
    if (t.closest("[data-chat]")) return discuss();
    if (t.closest("[data-next]")) return next(tab);
    if (t.closest(".ask-other")) { cursor = options(tab).length; return; }
    const o = t.closest(".ask-opt[data-k]"); if (o) { cursor = +o.dataset.k; choose(tab, cursor); }
  });
  card.addEventListener("input", (e) => {
    if (!e.target.classList.contains("ask-other")) return;
    other[tab] = e.target.value;
    if (!qs[tab].multiSelect && e.target.value.trim()) picked[tab].clear();
    const nb = card.querySelector("[data-next]"); if (nb) nb.disabled = !done(tab);
    const box = card.querySelectorAll(".ask-box")[tab]; if (box) box.textContent = done(tab) ? "☒" : "☐";
  });
  card.addEventListener("keydown", (e) => {
    if (busy || e.isComposing) return;
    const inOther = e.target.classList?.contains("ask-other");
    if (e.key === "Escape") { e.preventDefault(); return inOther ? card.focus() : cancel(); }
    if (inOther) {
      if (e.key === "Enter") { e.preventDefault(); if (done(tab)) next(tab); }
      else if (e.key === "ArrowUp") { e.preventDefault(); cursor = options(tab).length - 1; card.focus(); draw(); }
      return;
    }
    if (tab === qs.length) { // récapitulatif
      if (e.key === "Enter") { e.preventDefault(); send(); }
      else if (e.key === "ArrowLeft" || (e.key === "Tab" && e.shiftKey)) { e.preventDefault(); go(tab - 1); }
      return;
    }
    const n = options(tab).length;
    const focusOther = () => { draw(); card.querySelector(".ask-other")?.focus(); };
    if (/^[1-9]$/.test(e.key) && +e.key <= n + 1) { e.preventDefault(); cursor = +e.key - 1; return cursor === n ? focusOther() : choose(tab, cursor); }
    if (e.key === "ArrowDown") { e.preventDefault(); cursor = Math.min(cursor + 1, n); return cursor === n ? focusOther() : draw(); }
    if (e.key === "ArrowUp") { e.preventDefault(); cursor = Math.max(cursor - 1, 0); return draw(); }
    if (e.key === "Enter") { e.preventDefault(); return cursor === n ? focusOther() : choose(tab, cursor); }
    if (many && (e.key === "ArrowRight" || (e.key === "Tab" && !e.shiftKey))) { e.preventDefault(); return go(tab + 1); }
    if (many && (e.key === "ArrowLeft" || (e.key === "Tab" && e.shiftKey))) { e.preventDefault(); return go(tab - 1); }
  });
  draw();
  // Focus seulement si la carte attend encore (au rejeu, sa réponse la referme aussitôt).
  requestAnimationFrame(() => { if (card.querySelector(".ask-body")) { card.focus({ preventScroll: true }); card.scrollIntoView({ block: "nearest", behavior: "smooth" }); } });
}

/** Carte close : les réponses données, ou le refus. */
export function askResolved(req, ev) {
  const qs = req?.input?.questions || [];
  if (!ev.allow) return `<div class="ask-done no">${ev.message?.includes("discuss") ? "Tu as choisi d'en discuter." : "Questions annulées."}</div>`;
  const a = ev.answers || {};
  return `<ul class="ask-review done">${qs.map((q) => `<li><div>${esc(q.question)}</div><div class="ask-ans">→ ${esc(a[q.question] || "sans réponse")}</div></li>`).join("")}</ul>`;
}
