// Questions à choix de Claude (`AskUserQuestion`), comme dans le terminal : un onglet par
// question, des choix numérotés avec leur description, « Autre » en texte libre, cases à
// cocher pour un choix multiple, puis un récapitulatif avant l'envoi (plusieurs questions).
// Clavier : 1–9 choisit, ↑/↓ déplace, Entrée valide, ←/→ ou Tab change d'onglet, Échap annule.
// `askModel` porte l'état et les règles (sans DOM, testé) ; `mountAsk` l'affiche.
import { esc } from "./text.js";

/** État des réponses aux questions `qs` et leurs règles. */
export function askModel(qs) {
  const picked = qs.map(() => new Set()); // choix cochés (libellés)
  const other = qs.map(() => "");         // texte libre (« Autre »)
  const m = {
    qs, picked, other,
    many: qs.length > 1,
    options: (i) => qs[i]?.options || [],
    /** Réponse de la question i : choix puis texte libre, séparés par des virgules. */
    value: (i) => [...picked[i], ...(other[i].trim() ? [other[i].trim()] : [])].join(", "),
    done: (i) => !!m.value(i),
    /** Choisit l'option k de la question i. Rend true si la question est close (choix unique). */
    choose(i, k) {
      const label = m.options(i)[k]?.label;
      if (label == null) return false;
      if (qs[i].multiSelect) {
        picked[i].has(label) ? picked[i].delete(label) : picked[i].add(label);
        return false;
      }
      picked[i] = new Set([label]);
      other[i] = "";
      return true;
    },
    /** Texte libre : en choix unique, il remplace le choix coché. */
    setOther(i, text) {
      other[i] = text;
      if (!qs[i].multiSelect && text.trim()) picked[i].clear();
    },
    /** Onglet qui suit la question i : la prochaine sans réponse, sinon le récapitulatif. */
    nextTab(i) {
      const j = qs.findIndex((_, j) => j > i && !m.done(j));
      return j >= 0 ? j : qs.length;
    },
    answers: () => Object.fromEntries(qs.map((q, i) => [q.question, m.value(i)])),
  };
  return m;
}

/**
 * Monte la carte dans `card` (un élément vide) pour la demande `req` (`can_use_tool`).
 * `answer(answers)` : envoie les réponses (question → choix) ; `refuse(message)` : refuse,
 * avec le motif que Claude lira ; `chat()` : l'utilisateur préfère en parler (saisie).
 */
export function mountAsk(card, req, { answer, refuse, chat }) {
  const m = askModel(req.input?.questions || []);
  const { qs, many } = m;
  let tab = 0;      // question affichée ; qs.length = récapitulatif
  let cursor = 0;   // choix en surbrillance (options(tab).length = « Autre »)
  let busy = false;

  const once = (p) => { if (busy) return; busy = true; p().catch(() => { busy = false; }); };
  const send = () => once(() => answer(m.answers()));
  const cancel = () => once(() => refuse("The user declined to answer these questions."));
  const discuss = () => once(() => refuse("The user wants to discuss these questions in the chat instead of choosing an answer. Wait for their message.").then(chat));

  const go = (t) => { tab = Math.max(0, Math.min(t, many ? qs.length : 0)); cursor = 0; draw(); card.focus({ preventScroll: true }); };
  const next = (i) => (many ? go(m.nextTab(i)) : send());
  const choose = (i, k) => { if (m.choose(i, k)) next(i); else draw(); };
  // Surbrillance sans redessiner (le champ « Autre » garde le focus et le curseur).
  const markCursor = () => {
    card.querySelectorAll(".ask-opt[data-k]").forEach((o) => o.classList.toggle("cur", +o.dataset.k === cursor));
    if (!qs[tab]?.multiSelect && m.other[tab].trim()) card.querySelectorAll(".ask-opt.sel").forEach((o) => o.classList.remove("sel"));
  };

  const tabsHtml = () => !many ? "" : `<div class="ask-tabs">${qs.map((q, i) => `<button type="button" class="ask-tab${i === tab ? " on" : ""}" data-tab="${i}"><span class="ask-box">${m.done(i) ? "☒" : "☐"}</span>${esc(q.header || `Question ${i + 1}`)}</button>`).join("")}<button type="button" class="ask-tab${tab === qs.length ? " on" : ""}" data-tab="${qs.length}">✓ Envoyer</button></div>`;

  const questionHtml = (i) => {
    const q = qs[i], opts = m.options(i), n = opts.length;
    const row = (k, inner, cls = "") => `<div class="ask-opt${k === cursor ? " cur" : ""}${cls}" data-k="${k}"><span class="ask-n">${k + 1}.</span>${inner}</div>`;
    return `<div class="ask-q">${esc(q.question)}${q.multiSelect ? ` <span class="muted">(plusieurs choix possibles)</span>` : ""}</div>
      <div class="ask-opts">
        ${opts.map((o, k) => row(k, `${q.multiSelect ? `<span class="ask-check">${m.picked[i].has(o.label) ? "☒" : "☐"}</span>` : ""}<div><div class="ask-label">${esc(o.label)}</div>${o.description ? `<div class="ask-desc">${esc(o.description)}</div>` : ""}</div>`, !q.multiSelect && m.picked[i].has(o.label) ? " sel" : "")).join("")}
        ${row(n, `<input class="ask-other" type="text" placeholder="Autre réponse…" value="${esc(m.other[i])}">`)}
      </div>
      <div class="ask-foot">
        ${q.multiSelect || many ? `<button type="button" class="btn primary" data-next ${m.done(i) ? "" : "disabled"}>${many ? "Suivant" : "Envoyer"}</button>` : ""}
        <button type="button" class="link" data-chat>En discuter avec Claude</button>
        <button type="button" class="link" data-cancel>Annuler</button>
        <span class="ask-keys">1–${n + 1} choisir · ↑↓ Entrée${many ? " · ←→ onglets" : ""} · Échap annuler</span>
      </div>`;
  };

  const reviewHtml = () => `<div class="ask-q">Vérifie tes réponses</div>
    <ul class="ask-review">${qs.map((q, i) => `<li><div>${esc(q.question)}</div><div class="${m.done(i) ? "ask-ans" : "ask-miss"}">→ ${m.done(i) ? esc(m.value(i)) : "sans réponse"}</div></li>`).join("")}</ul>
    <div class="ask-foot"><button type="button" class="btn primary" data-send>Envoyer les réponses</button><button type="button" class="link" data-chat>En discuter avec Claude</button><button type="button" class="link" data-cancel>Annuler</button></div>`;

  function draw() {
    const keepOther = document.activeElement?.classList.contains("ask-other");
    card.innerHTML = `${tabsHtml()}<div class="ask-body">${tab < qs.length ? questionHtml(tab) : reviewHtml()}</div>`;
    if (keepOther) { const inp = card.querySelector(".ask-other"); inp?.focus(); inp?.setSelectionRange(inp.value.length, inp.value.length); }
  }
  const focusOther = () => { cursor = m.options(tab).length; draw(); card.querySelector(".ask-other")?.focus(); };

  card.tabIndex = 0;
  card.addEventListener("click", (e) => {
    const t = e.target;
    const tb = t.closest("[data-tab]"); if (tb) return go(+tb.dataset.tab);
    if (t.closest("[data-send]")) return send();
    if (t.closest("[data-cancel]")) return cancel();
    if (t.closest("[data-chat]")) return discuss();
    if (t.closest("[data-next]")) return next(tab);
    if (t.closest(".ask-other")) return;
    const o = t.closest(".ask-opt[data-k]");
    if (!o) return;
    cursor = +o.dataset.k;
    if (cursor === m.options(tab).length) return focusOther(); // clic à côté du champ « Autre »
    choose(tab, cursor);
  });
  // Entrer dans « Autre » l'allume ; y écrire éteint en plus le choix coché d'un choix unique.
  card.addEventListener("focusin", (e) => { if (e.target.classList.contains("ask-other")) { cursor = m.options(tab).length; markCursor(); } });
  card.addEventListener("input", (e) => {
    if (!e.target.classList.contains("ask-other")) return;
    m.setOther(tab, e.target.value);
    cursor = m.options(tab).length;
    markCursor();
    const nb = card.querySelector("[data-next]"); if (nb) nb.disabled = !m.done(tab);
    const box = card.querySelectorAll(".ask-box")[tab]; if (box) box.textContent = m.done(tab) ? "☒" : "☐";
  });
  card.addEventListener("keydown", (e) => {
    if (busy || e.isComposing) return;
    const inOther = e.target.classList?.contains("ask-other");
    if (e.key === "Escape") { e.preventDefault(); return inOther ? card.focus() : cancel(); }
    if (inOther) {
      if (e.key === "Enter") { e.preventDefault(); if (m.done(tab)) next(tab); }
      else if (e.key === "ArrowUp") { e.preventDefault(); cursor = m.options(tab).length - 1; card.focus(); draw(); }
      return;
    }
    if (tab === qs.length) { // récapitulatif
      if (e.key === "Enter") { e.preventDefault(); send(); }
      else if (e.key === "ArrowLeft" || (e.key === "Tab" && e.shiftKey)) { e.preventDefault(); go(tab - 1); }
      return;
    }
    const n = m.options(tab).length;
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
