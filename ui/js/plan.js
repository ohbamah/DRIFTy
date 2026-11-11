// Plan proposé par Claude (`ExitPlanMode`) : le plan lisible, puis l'approuver ou continuer à
// planifier avec une remarque. Approuver sans mode ramène Claude au mode d'avant le plan
// (mesuré le 2026-11-10 : une discussion bypass y revient) ; `acceptEdits` accepte les
// modifications d'office, comme le premier choix du terminal.
import { esc } from "./text.js";
import { md } from "./md.js";

/** Les choix d'approbation selon la discussion : bypass, ou avec autorisations. */
export function planChoices(bypass) {
  return bypass
    ? [{ label: "Approuver le plan", mode: null, hint: "Claude reprend en bypass" }]
    : [
        { label: "Approuver, modifications acceptées d'office", mode: "acceptEdits", hint: "Claude modifie les fichiers sans te demander" },
        { label: "Approuver, en validant chaque modification", mode: null, hint: "Claude te demande avant chaque modification" },
      ];
}

/** Motif lu par Claude quand on continue à planifier. */
export function keepPlanningMessage(feedback) {
  const f = (feedback || "").trim();
  return f ? `The user wants to keep planning. Revise the plan with this feedback: ${f}` : "The user wants to keep planning. Revise the plan before asking again.";
}

/** Issue d'un plan (carte close). */
export function planOutcome(ev, choices) {
  if (ev.allow) return `Plan approuvé${choices.length > 1 ? ` : ${(choices.find((c) => c.mode === (ev.mode ?? null)) || choices[0]).label.replace(/^Approuver, /, "")}` : ""}.`;
  const m = /feedback: ([\s\S]*)$/.exec(ev.message || "");
  return m ? `Claude continue à planifier, avec ta remarque : « ${m[1]} »` : "Claude continue à planifier.";
}

/** Monte la carte dans `card` ; `approve(mode)`, `keep(message)` renvoient une promesse. */
export function mountPlan(card, req, { choices, approve, keep }) {
  const plan = req.input?.plan || "";
  const file = req.input?.planFilePath;
  card.innerHTML = `<div class="plan-head"><b>Plan proposé par Claude</b>${file ? `<span class="plan-file" title="${esc(file)}">${esc(file.split("/").pop())}</span>` : ""}</div>
    <div class="plan-body msg-ai">${md(plan)}</div>
    <div class="plan-actions">
      ${choices.map((c, i) => `<button type="button" class="btn${i ? "" : " primary"}" data-mode="${c.mode ?? ""}" title="${esc(c.hint)}">${i + 1}. ${esc(c.label)}</button>`).join("")}
      <button type="button" class="btn" data-keep>${choices.length + 1}. Continuer à planifier</button>
      <div class="plan-keep" hidden><textarea class="field area" rows="2" placeholder="Ce qu'il faut changer (facultatif)…"></textarea><button type="button" class="btn primary" data-keep-send>Renvoyer à Claude</button></div>
    </div>`;
  let busy = false;
  const run = (p) => { if (busy) return; busy = true; card.querySelectorAll("button").forEach((b) => (b.disabled = true)); p().catch(() => { busy = false; card.querySelectorAll("button").forEach((b) => (b.disabled = false)); }); };
  const openKeep = () => { const k = card.querySelector(".plan-keep"); k.hidden = false; k.querySelector("textarea").focus(); };
  const sendKeep = () => run(() => keep(keepPlanningMessage(card.querySelector(".plan-keep textarea").value)));
  card.addEventListener("click", (e) => {
    const b = e.target.closest("button");
    if (!b) return;
    if (b.hasAttribute("data-mode")) run(() => approve(b.dataset.mode || null));
    else if (b.hasAttribute("data-keep")) openKeep();
    else if (b.hasAttribute("data-keep-send")) sendKeep();
  });
  card.addEventListener("keydown", (e) => {
    if (busy) return;
    if (e.target.tagName === "TEXTAREA") { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); sendKeep(); } return; }
    const n = +e.key;
    if (n >= 1 && n <= choices.length) { e.preventDefault(); run(() => approve(choices[n - 1].mode)); }
    else if (n === choices.length + 1) { e.preventDefault(); openKeep(); }
  });
  card.tabIndex = 0;
  requestAnimationFrame(() => { if (card.querySelector(".plan-actions button")) { card.focus({ preventScroll: true }); card.scrollIntoView({ block: "nearest", behavior: "smooth" }); } });
}
