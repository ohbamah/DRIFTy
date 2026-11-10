// Bilan : ce que Claudius fait économiser face à ce qu'il dépense lui-même.
import { api, esc, tok, num, pct, usd, syncButton, toast, logo, $ } from "../lib.js";
import { store, ensureLoaded } from "../store.js";

const KIND = { improve: "Amélioration de prompt", evaluate: "Évaluation par agent" };
import { bars } from "../chart.js";

// Balance : penche du côté le plus lourd (échelle log, bornée à 14°).
function beam(left, right) {
  const ratio = Math.log10((left + 1) / (right + 1));
  const deg = Math.max(-14, Math.min(14, ratio * 3));
  return `<svg viewBox="0 0 220 150" aria-hidden="true">
    <path d="M110 30 V130 M80 134 H140" stroke="var(--muted)" stroke-width="2" stroke-linecap="round"/>
    <g transform="rotate(${-deg} 110 30)">
      <line x1="20" y1="30" x2="200" y2="30" stroke="var(--ink-2)" stroke-width="3" stroke-linecap="round"/>
      <g transform="rotate(${deg} 20 30)"><path d="M20 30 L4 72 M20 30 L36 72" stroke="var(--muted)" stroke-width="1.5"/><path d="M0 72 H40 A20 9 0 0 1 0 72 Z" fill="var(--gain)"/></g>
      <g transform="rotate(${deg} 200 30)"><path d="M200 30 L184 72 M200 30 L216 72" stroke="var(--muted)" stroke-width="1.5"/><path d="M180 72 H220 A20 9 0 0 1 180 72 Z" fill="var(--s1)"/></g>
    </g>
    <circle cx="110" cy="30" r="5" fill="var(--ink)"/>
  </svg>`;
}

export async function show(root) {
  const l = await api("/api/ledger");
  const spent = l.spend.input + l.spend.output;
  const realized = l.saved_tokens;
  const left = realized ?? l.identified_tokens;
  const since = new Date(l.installed_at).toLocaleDateString("fr-FR", { day: "numeric", month: "long" });
  const weeks = l.weeks.slice(-16).map((w) => {
    const d = new Date(w.week_start);
    return {
      label: d.toLocaleDateString("fr-FR", { day: "2-digit", month: "2-digit" }),
      title: `Semaine du ${d.toLocaleDateString("fr-FR", { day: "numeric", month: "long" })} · ${w.sessions} conversations`,
      failed: w.context ? w.failed / w.context : 0,
      repeats: w.context ? w.repeats / w.context : 0,
      big: w.context ? w.big / w.context : 0,
    };
  });

  root.innerHTML = `<div class="page"><div class="page-in">
    <div class="page-head"><div><h1>Bilan</h1><div class="sub">Économies face au coût de Claudius.</div></div>${syncButton()}</div>

    <div class="panel ledger">
      <div class="side">
        <div class="k">${realized != null ? "Économisé depuis le " + esc(since) : "Économisable"}</div>
        <div class="v gain-v">${tok(left)}</div>
        <div class="muted small">tokens</div>
      </div>
      <div class="beam">${beam(left, spent)}</div>
      <div class="side right">
        <div class="k">Dépensé par Claudius</div>
        <div class="v">${tok(spent)}</div>
        <div class="muted small">${l.spend.runs ? `tokens, ${usd(l.spend.cost_usd)} sur ${num(l.spend.runs)} appels` : "tokens, analyses locales"}</div>
      </div>
    </div>

    <div class="grid">
      <div class="panel">
        <h2>Économie réalisée</h2>
        ${realized != null
          ? `<div class="note">Gaspillage : <b>${pct(l.before.rate, 2)}</b> → <b>${pct(l.after.rate, 2)}</b>, soit <b style="color:var(--gain)">${tok(realized)}</b> tokens en moins.</div>`
          : `<div class="note">Mesurable après ${l.min_sessions_after} conversations (${l.after.sessions}/${l.min_sessions_after}). Gaspillage actuel : ${pct(l.before.rate, 2)} du contexte.</div>`}
      </div>
      <div class="panel">
        <h2>Méthode</h2>
        <div class="note">Gaspillage = échecs d'outil, répétitions et grosses sorties relues. Économie = baisse de ce taux depuis la mise en service.</div>
      </div>
    </div>

    ${l.spend_by_kind.length ? `<div class="panel">
      <h2>Dépenses de Claudius</h2>
      <table class="data">
        <tr><th>Usage</th><th class="n">Appels</th><th class="n">Tokens</th><th class="n">Coût</th></tr>
        ${l.spend_by_kind.map((k) => `<tr><td>${esc(KIND[k.kind] || k.kind)}</td><td class="n">${num(k.runs)}</td><td class="n">${tok(k.input + k.output)}</td><td class="n">${usd(k.cost_usd)}</td></tr>`).join("")}
      </table>
    </div>` : ""}

    <div class="panel" id="rules-panel">
      <h2>Effet des règles du CLAUDE.md</h2>
      <div id="rules" class="muted small">Chargement…</div>
    </div>

    <div class="panel">
      <h2>Gaspillage par semaine</h2>
      <div id="w"></div>
    </div>
  </div></div>`;
  $("#w").append(bars(weeks, [
    { key: "failed", label: "Échecs d'outil", color: "var(--s4)" },
    { key: "repeats", label: "Répétitions", color: "var(--s2)" },
    { key: "big", label: "Grosses sorties", color: "var(--s1)" },
  ], { fmt: (v) => pct(v, 1), height: 240, width: $("#w").clientWidth }));
  drawRules($("#rules"));
}

// ------------------------------------------------------------ effet des règles
const VERDICT = {
  "efficace": "ok", "partiel": "mid", "sans effet": "bad", "trop tôt": "wait", "rien à mesurer": "wait", "inconnu": "wait",
};
const fmtRate = (r) => (r.unit === "part du contexte" ? pct(r.before, 2) + " → " + pct(r.after, 2) : `${r.before.toFixed(1)} → ${r.after.toFixed(1)}`);

async function drawRules(box) {
  await ensureLoaded();
  const repos = store.repos.filter((r) => !r.hidden);
  const lists = await Promise.all(repos.map((r) => api(`/api/repos/${r.id}/rules`).then((d) => ({ repo: r, ...d })).catch(() => null)));
  const withRules = lists.filter((l) => l && l.rules.length);
  if (!withRules.length) { box.textContent = "Aucune règle appliquée pour l'instant."; return; }
  box.classList.remove("muted", "small");
  box.innerHTML = withRules.map((l) => `
    <div class="rules-proj"><div class="rules-head">${logo(l.repo, "sm")}<b>${esc(l.repo.name)}</b><span class="muted small">${esc(l.path.replace(/^\/Users\/[^/]+/, "~"))}</span></div>
    <table class="data">
      <tr><th>Règle</th><th>Verdict</th><th class="n">Avant → après</th><th class="n">Sessions après</th><th></th></tr>
      ${l.rules.map((r) => `<tr>
        <td class="rule-cell" title="${esc(r.rule)}">${esc(r.rule)}</td>
        <td><span class="verdict-chip ${VERDICT[r.verdict] || "wait"}">${esc(r.verdict)}</span></td>
        <td class="n">${r.before != null ? `${fmtRate(r)} <span class="muted small">${esc(r.unit)}</span>` : "—"}</td>
        <td class="n">${r.after_sessions != null ? `${r.after_sessions}${r.after_sessions < r.min_sessions ? ` / ${r.min_sessions}` : ""}` : "—"}</td>
        <td class="n"><button class="btn${r.verdict === "sans effet" ? " primary" : ""}" data-rm="${esc(r.key)}" data-repo="${l.repo.id}">Retirer</button></td>
      </tr>`).join("")}
    </table></div>`).join("");
  box.querySelectorAll("[data-rm]").forEach((b) => b.addEventListener("click", async () => {
    if (!confirm("Retirer cette règle du CLAUDE.md du projet ?")) return;
    try {
      await api(`/api/repos/${b.dataset.repo}/rules/${encodeURIComponent(b.dataset.rm)}`, { method: "DELETE" });
      toast("Règle retirée du CLAUDE.md.");
      drawRules(box);
    } catch (e) { toast(e.message, true); }
  }));
}
