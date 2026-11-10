// Onglet « Analyse » d'une conversation : temps, causes, gains estimés.
import { api, esc, tok, dur, pct, num, usd, date, showTip, hideTip, loader, toast } from "../lib.js";
import { md } from "../md.js";

const VERDICT = { oui: "Optimisable", partiel: "En partie", non: "Non optimisable" };
const TIME = [
  { key: "model_ms", label: "Modèle", color: "var(--s1)" },
  { key: "tool_ms", label: "Outils", color: "var(--s2)" },
  { key: "human_ms", label: "Ton attente", color: "var(--s3)" },
  { key: "pause_ms", label: "Pauses", color: "var(--line)" },
];
const COLS = {
  ts: "Quand", ms: "Durée", name: "Outil", target: "Cible", error: "Erreur", count: "Fois", tokens: "Tokens",
  bytes: "Taille", model: "Modèle", thinking: "Réflexion", output: "Sortie", context: "Contexte", text: "Texte",
};
const NUMERIC = new Set(["ms", "count", "tokens", "bytes", "thinking", "output", "context"]);

function cell(k, v) {
  if (k === "ts") return date(v);
  if (k === "ms") return dur(v);
  if (k === "bytes") return tok(v) + "o";
  if (NUMERIC.has(k)) return tok(v);
  if (typeof v === "boolean") return v ? "échec" : "";
  return esc(v);
}

function items(list) {
  if (!list.length) return "";
  const cols = Object.keys(list[0]);
  return `<div class="scroll-x"><table class="data" style="margin-top:8px">
    <tr>${cols.map((k) => `<th class="${NUMERIC.has(k) ? "n" : ""}">${COLS[k] || esc(k)}</th>`).join("")}</tr>
    ${list.map((it) => `<tr>${cols.map((k) => {
      const cls = NUMERIC.has(k) ? "n" : ["target", "error", "text"].includes(k) ? "clip code" : "";
      const title = typeof it[k] === "string" ? ` title="${esc(it[k])}"` : "";
      return `<td class="${cls}"${title}>${cell(k, it[k])}</td>`;
    }).join("")}</tr>`).join("")}
  </table></div>`;
}

function cause(c, i, fixable) {
  const nums = [c.ms >= 1000 ? dur(c.ms) : "", c.tokens ? tok(c.tokens) + " tokens" : ""].filter(Boolean).join(", ");
  return `<div class="cause">
    <div class="cause-top"><span class="t">${esc(c.title)}</span><span class="n">${nums}</span><span class="verdict ${c.fixed && !c.fixed.session_started_before ? "fixed" : c.optimisable}">${c.fixed && !c.fixed.session_started_before ? "Optimisé" : VERDICT[c.optimisable]}</span></div>
    <p>${esc(c.why)}</p>
    ${c.fixed ? `<div class="fixed-note">
      <b>Optimisé le ${date(c.fixed.at)}.</b>
      ${c.fixed.session_started_before
        ? " Cette session a démarré avant : elle ne lit pas les nouvelles règles (le CLAUDE.md est lu au démarrage), elles s'appliqueront à la prochaine."
        : ""}
      Depuis : ${c.fixed.new_tokens || c.fixed.new_ms >= 1000 ? `${[c.fixed.new_tokens ? tok(c.fixed.new_tokens) + " tokens" : "", c.fixed.new_ms >= 1000 ? dur(c.fixed.new_ms) : ""].filter(Boolean).join(", ")}` : "rien"}.
    </div>` : ""}
    <div style="display:flex;gap:8px">
      ${fixable ? `<button class="btn primary" data-fix="${i}">Optimiser</button>` : ""}
      ${c.gain ? `<button class="btn${fixable ? "" : " primary"}" data-gain="${i}">Voir les gains</button>` : ""}
      ${c.items.length ? `<button class="btn" data-items="${i}">Détails</button>` : ""}
    </div>
    <div class="more" hidden></div>
  </div>`;
}

// `code` → <code>, après échappement.
const inlineCode = (t) => esc(t).replace(/`([^`]+)`/g, "<code>$1</code>");

function fixBlock(p) {
  const file = String(p.path).replace(/^\/Users\/[^/]+/, "~");
  if (!p.entries.length) {
    return `<div class="fix"><div class="muted small">Aucune règle concrète à tirer de cette conversation.</div></div>`;
  }
  const todo = p.entries.filter((e) => e.status !== "present");
  const LABEL = { new: "Nouvelle", updated: "Mise à jour", present: "Déjà là" };
  return `<div class="fix">
    <div class="fix-head"><code>${esc(file)}</code>${p.file_exists ? "" : " (créé)"}, bloc Claudius : ${p.block_rules} règle${p.block_rules > 1 ? "s" : ""} sur ${p.block_max} au plus</div>
    <ul class="rules">
      ${p.entries.map((e) => `<li class="rule ${e.status === "present" ? "done" : ""}" data-raw="${esc(e.text)}"><span class="rule-tag">${LABEL[e.status]}</span>${inlineCode(e.text)}</li>`).join("")}
    </ul>
    <div style="display:flex;gap:8px;align-items:center">
      ${todo.length ? `<button class="btn primary" data-apply>Appliquer</button><button class="btn" data-copy>Copier</button>` : `<span class="small" style="color:var(--gain)">Déjà appliqué.</span>`}
    </div>
  </div>`;
}

function gainBlock(g) {
  return `<div class="gain">
    ${g.tokens ? `<div><span class="muted small">Tokens</span><b>${tok(g.tokens)}</b></div>` : ""}
    ${g.ms >= 1000 ? `<div><span class="muted small">Temps</span><b>${dur(g.ms)}</b></div>` : ""}
    <div class="basis">${esc(g.basis)} (1 token ≈ 4 octets)</div>
  </div>`;
}

export async function renderAnalysis(root, sid) {
  root.innerHTML = loader();
  const [s, d] = await Promise.all([api(`/api/sessions/${sid}`), api(`/api/sessions/${sid}/digest`)]);
  const t = d.time;
  const total = TIME.reduce((a, p) => a + (t[p.key] || 0), 0) || 1;
  root.innerHTML = `<div class="inset">
    <div class="figures">
      <div class="fig"><div class="k">Coût équivalent API</div><div class="v">${usd(s.cost_usd)}</div><div class="s">${s.lines_added != null ? `+${num(s.lines_added)} / −${num(s.lines_removed)} lignes` : "—"}</div></div>
      <div class="fig"><div class="k">Appels au modèle</div><div class="v">${num(d.counts.api_calls)}</div><div class="s">${d.counts.subagents ? `${d.counts.subagents} sous-agents` : "aucun sous-agent"}</div></div>
      <div class="fig"><div class="k">Contexte initial</div><div class="v">${tok(d.tokens.first_context)}</div><div class="s">jusqu'à ${tok(d.tokens.max_context)}</div></div>
      <div class="fig"><div class="k">Servi par le cache</div><div class="v">${pct(d.tokens.cache_ratio)}</div><div class="s">${tok(d.tokens.cache_read)} relus</div></div>
      <div class="fig"><div class="k">Économisable</div><div class="v" style="color:var(--gain)">${tok(d.gain_total.tokens)}</div><div class="s">tokens${d.gain_total.ms >= 1000 ? `, ${dur(d.gain_total.ms)}` : ""}</div></div>
    </div>

    <div class="panel" id="eval-panel"></div>

    <div class="panel">
      <h2>Temps</h2>
      <div class="legend">${TIME.map((p) => `<span><i style="background:${p.color}"></i>${p.label} ${dur(t[p.key])}</span>`).join("")}</div>
      <div class="timebar">${TIME.filter((p) => t[p.key] > 0).map((p) => `<div data-k="${p.key}" style="flex:${t[p.key] / total};background:${p.color}"></div>`).join("")}</div>
      <div class="muted small">${t.model_official && t.tool_official ? "Durées relevées par Claude Code." : "Durées estimées."} Pause : plus de 30 min sans activité.</div>
    </div>

    <div class="panel">
      <h2>Causes</h2>
      ${d.causes.map((c, i) => cause(c, i, d.fixable.includes(c.id))).join("") || `<div class="muted">Rien à signaler.</div>`}
    </div>

    ${d.corrections.length ? `<div class="panel">
      <h2>Tes recadrages</h2>
      <div class="muted small" style="margin:-8px 0 10px">Repérés par mots-clés.</div>
      <table class="data">${d.corrections.slice(0, 20).map((c) => `<tr><td class="muted" style="white-space:nowrap">${date(c.ts)}</td><td>${esc(c.text)}</td></tr>`).join("")}</table>
    </div>` : ""}
  </div>`;

  root.querySelectorAll(".timebar div").forEach((el) => {
    const p = TIME.find((x) => x.key === el.dataset.k);
    el.addEventListener("mousemove", (e) => showTip(e, `<b>${p.label}</b><div class="row"><span>${dur(t[p.key])}</span><span>${pct(t[p.key] / total, 0)}</span></div>`));
    el.addEventListener("mouseleave", hideTip);
  });
  const open = (btn, html, mode) => {
    const box = btn.closest(".cause").querySelector(".more");
    if (!box.hidden && box.dataset.mode === mode) return (box.hidden = true);
    box.dataset.mode = mode;
    box.innerHTML = html;
    box.hidden = false;
  };
  root.querySelectorAll("[data-gain]").forEach((b) => b.addEventListener("click", () => {
    const c = d.causes[+b.dataset.gain];
    open(b, gainBlock(c.gain) + items(c.items), "gain");
  }));
  root.querySelectorAll("[data-fix]").forEach((b) => b.addEventListener("click", async () => {
    const c = d.causes[+b.dataset.fix];
    const box = b.closest(".cause").querySelector(".more");
    if (!box.hidden && box.dataset.mode === "fix") return (box.hidden = true);
    box.dataset.mode = "fix";
    box.hidden = false;
    box.innerHTML = loader(true);
    try {
      box.innerHTML = fixBlock(await api(`/api/sessions/${sid}/fix/${encodeURIComponent(c.id)}`));
    } catch (e) { box.innerHTML = `<div class="err small">${esc(e.message)}</div>`; return; }
    box.querySelector("[data-apply]")?.addEventListener("click", async (ev) => {
      ev.target.disabled = true;
      try {
        const r = await api(`/api/sessions/${sid}/fix/${encodeURIComponent(c.id)}`, { method: "POST" });
        toast(`CLAUDE.md mis à jour (${r.changed} règle${r.changed > 1 ? "s" : ""}).`);
        box.innerHTML = fixBlock(await api(`/api/sessions/${sid}/fix/${encodeURIComponent(c.id)}`));
      } catch (e) { toast(e.message, true); ev.target.disabled = false; }
    });
    box.querySelector("[data-copy]")?.addEventListener("click", async () => {
      const text = [...box.querySelectorAll(".rule:not(.done)[data-raw]")].map((r) => "- " + r.dataset.raw).join("\n");
      try { await navigator.clipboard.writeText(text); toast("Copié."); } catch { toast("Copie impossible.", true); }
    });
  }));
  root.querySelectorAll("[data-items]").forEach((b) => b.addEventListener("click", () => open(b, items(d.causes[+b.dataset.items].items), "items")));
  drawEval(root.querySelector("#eval-panel"), sid);
}

// ------------------------------------------------------------ évaluation par agent
async function drawEval(box, sid) {
  if (!box) return;
  const ev = await api(`/api/sessions/${sid}/evaluation`).catch(() => null);
  box.innerHTML = `<div class="eval-head"><h2 style="margin:0">Évaluation par agent</h2>
      ${ev ? `<span class="muted small">${date(ev.ts)} · ${tok(ev.tokens)} tokens · ${usd(ev.cost_usd)}</span>` : ""}
      <span style="flex:1"></span>
      <button class="btn${ev ? "" : " primary"}" data-eval>${ev ? "Réévaluer" : "Évaluer avec un agent"}</button></div>
    ${ev ? `<div class="msg-ai eval-report">${md(ev.report)}</div>` : `<div class="muted small">Un agent lit le résumé chiffré de cette conversation (jamais le transcript) et propose des améliorations concrètes. Quelques centimes, comptés dans le Bilan.</div>`}`;
  box.querySelector("[data-eval]").addEventListener("click", async (e) => {
    e.target.disabled = true;
    e.target.innerHTML = `<span class="spin" style="width:14px;height:14px;border-width:2px"></span> Évaluation…`;
    try { await api(`/api/sessions/${sid}/evaluation`, { method: "POST" }); toast("Évaluation terminée."); drawEval(box, sid); }
    catch (err) { toast(err.message, true); e.target.disabled = false; e.target.textContent = "Réessayer"; }
  });
}
