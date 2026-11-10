// Analytique : usage global, filtrable par compte et par période.
import { api, esc, tok, num, pct, usd, logo, loader, $ } from "../lib.js";
import { bars } from "../chart.js";
import { slide, moveTo } from "../slide.js";

const f = { days: "30", slot: "" };
try { Object.assign(f, JSON.parse(localStorage.getItem("drifty.analytics") || "{}")); } catch {}

const seg = (name, opts, cur) =>
  `<div class="seg" data-seg="${name}">${opts.map(([v, l]) => `<button type="button" data-v="${v}" class="${v === cur ? "on" : ""}">${l}</button>`).join("")}</div>`;

const shortDay = (d) => d.slice(8, 10) + "/" + d.slice(5, 7);

/** Section « Activité » : filtres, chiffres, graphiques, tableaux. */
export async function render(root) {
  // Changement de filtre : l'en-tête reste (l'indicateur y glisse), les anciens chiffres disparaissent.
  const head = root.querySelector(":scope > .section-head");
  if (head) {
    [...root.children].forEach((el) => el !== head && el.remove());
    head.insertAdjacentHTML("afterend", loader());
  }
  const o = await api(`/api/overview?days=${f.days}&slot=${f.slot}`);
  const t = o.totals;
  const daily = o.daily.map((d) => ({ ...d, label: shortDay(d.day), title: d.day }));
  root.innerHTML = `
    <div class="page-head section-head">
      <div><h2 class="section-title">Activité</h2><div class="sub">${f.days === "0" ? "Depuis le début" : `${f.days} derniers jours`}, ${f.slot ? esc(f.slot) : "tous comptes"}</div></div>
      <div style="display:flex;gap:8px;flex-wrap:wrap">
        ${seg("slot", [["", "Tous"], ["perso", "Perso"], ["pro", "Pro"]], f.slot)}
        ${seg("days", [["7", "7 j"], ["30", "30 j"], ["90", "90 j"], ["0", "Tout"]], f.days)}
      </div>
    </div>
    <div class="figures">
      <div class="fig"><div class="k">Coût équivalent API</div><div class="v">${usd(o.cost_usd)}</div><div class="s">selon Claude Code</div></div>
      <div class="fig"><div class="k">Conversations</div><div class="v">${num(t.sessions)}</div><div class="s">${num(t.api_calls)} appels au modèle</div></div>
      <div class="fig"><div class="k">Servi par le cache</div><div class="v">${pct(t.cache_ratio)}</div><div class="s">${tok(t.cache_read)} tokens relus</div></div>
      <div class="fig"><div class="k">Écrit par le modèle</div><div class="v">${tok(t.output)}</div><div class="s">dont ${tok(t.thinking)} de réflexion</div></div>
      <div class="fig"><div class="k">Outils en échec</div><div class="v">${pct(o.tools.calls ? o.tools.errors / o.tools.calls : 0)}</div><div class="s">${num(o.tools.errors)} sur ${num(o.tools.calls)}</div></div>
    </div>
    <div class="grid">
      <div class="panel"><h2>Contexte relu (cache)</h2><div id="c1"></div></div>
      <div class="panel"><h2>Tokens neufs</h2><div id="c2"></div></div>
    </div>
    <div class="grid">
      <div class="panel scroll-x"><h2>Par projet</h2><table class="data">
        <tr><th>Projet</th><th class="n">Conversations</th><th class="n">Relu</th><th class="n">Neuf</th><th class="n">Coût</th></tr>
        ${o.projects.map((p) => `<tr><td class="clip" title="${esc(p.root)}"><a href="#/conversations?p=${p.id}" style="display:inline-flex;gap:8px;align-items:center">${logo(p, "sm")}${esc(p.name)}</a></td><td class="n">${p.sessions}</td><td class="n">${tok(p.cache_read)}</td><td class="n">${tok(p.fresh + p.output)}</td><td class="n">${usd(p.cost_usd)}</td></tr>`).join("")}
      </table></div>
      <div class="panel scroll-x"><h2>Par modèle</h2><table class="data">
        <tr><th>Modèle</th><th class="n">Appels</th><th class="n">Contexte</th><th class="n">Sortie</th></tr>
        ${o.models.map((m) => `<tr><td class="code">${esc(m.model)}</td><td class="n">${num(m.api_calls)}</td><td class="n">${tok(m.context)}</td><td class="n">${tok(m.output)}</td></tr>`).join("")}
      </table></div>
    </div>`;
  $("#c1").append(bars(daily, [{ key: "cache_read", label: "Relu", color: "var(--s1)" }], { width: $("#c1").clientWidth }));
  $("#c2").append(bars(daily, [
    { key: "cache_write", label: "Mis en cache", color: "var(--s1)" },
    { key: "input", label: "Entrée hors cache", color: "var(--s2)" },
    { key: "output", label: "Sortie", color: "var(--s3)" },
  ], { width: $("#c2").clientWidth }));
  slide(root);
  root.querySelectorAll("[data-seg] button").forEach((b) => b.addEventListener("click", () => {
    if (b.classList.contains("on")) return;
    f[b.closest("[data-seg]").dataset.seg] = b.dataset.v;
    try { localStorage.setItem("drifty.analytics", JSON.stringify(f)); } catch {}
    moveTo(b);
    render(root);
  }));
}
