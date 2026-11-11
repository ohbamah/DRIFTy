// Automations : éditeur de flows à la manière de n8n. Palette à gauche (on glisse un nœud sur
// le plan, ou on clique pour l'ajouter au centre), plan au milieu (glisser le fond pour se
// déplacer, molette pour zoomer, glisser une sortie ● vers un nœud pour les relier), réglages
// du nœud choisi à droite. Enregistré tout seul. L'exécution viendra quand les nœuds seront
// définis ensemble.
import { api, esc, icon, toast, $, $$ } from "../lib.js";
import { store, ensureLoaded } from "../store.js";
import { confirmBox } from "../confirm.js";
import { KINDS, PALETTE, newFlow, addNode, connect, removeNode, removeEdge, nodeOf, edgePath } from "../flow.js";

const NODE_W = 220;
let flows = [];
let flow = null;           // flow affiché
let sel = null;            // { node: id } | { edge: [from, to] }
let view = { x: 0, y: 0, k: 1 };
let saveTimer = 0;

const lastKey = "drifty.automation";
const remember = (id) => { try { localStorage.setItem(lastKey, id); } catch {} };
const recalled = () => { try { return localStorage.getItem(lastKey); } catch { return null; } };

export async function show(root) {
  await ensureLoaded();
  flows = await api("/api/automations").catch(() => []);
  if (!flows.length) { flows = [newFlow("Mon premier flow")]; await save(flows[0], true); }
  flow = flows.find((f) => f.id === recalled()) || flows[0];
  sel = null;
  view = { x: 0, y: 0, k: 1 };
  root.innerHTML = `<div class="af">
    <div class="af-head">
      <h1>Automations</h1>
      <select class="field af-pick" title="Flow affiché"></select>
      <input class="field af-name" maxlength="80" spellcheck="false" title="Nom du flow">
      <button class="btn" type="button" data-new>${icon("plus")}Nouveau flow</button>
      <button class="btn" type="button" data-del>Supprimer</button>
      <span style="flex:1"></span>
      <span class="af-status muted small"></span>
      <button class="btn primary" type="button" disabled title="L'exécution arrivera une fois les nœuds définis">${icon("play")}Exécuter</button>
    </div>
    <div class="af-body">
      <aside class="af-palette">
        <div class="af-pal-t">Nœuds</div>
        ${PALETTE.map((t) => `<div class="af-pal" draggable="true" data-kind="${t}" style="--tone:${KINDS[t].tone}" title="Glisser sur le plan, ou cliquer pour l'ajouter">${icon(KINDS[t].icon)}<span>${esc(KINDS[t].label)}</span></div>`).join("")}
        <div class="af-help muted small">Glisse un nœud sur le plan. Relie deux nœuds en tirant la pastille de droite de l'un vers l'autre. Suppr retire la sélection.</div>
      </aside>
      <div class="af-canvas" tabindex="0">
        <div class="af-world"><svg class="af-edges"></svg><div class="af-nodes"></div></div>
        <div class="af-zoom"><button type="button" data-zoom="-1" title="Dézoomer">−</button><button type="button" data-zoom="0" title="Voir tout le flow">${Math.round(view.k * 100)}%</button><button type="button" data-zoom="1" title="Zoomer">+</button></div>
      </div>
      <aside class="af-inspect"></aside>
    </div>
  </div>`;
  const el = $(".af", root);
  bindHead(el);
  bindPalette(el);
  bindCanvas(el);
  drawAll(el);
  fit(el);
}

// ------------------------------------------------------------ enregistrement
async function save(f, now = false) {
  const status = $(".af-status");
  clearTimeout(saveTimer);
  const go = async () => {
    try {
      const saved = await api(`/api/automations/${f.id}`, { method: "PUT", json: f });
      f.updated = saved.updated;
      if (status) status.textContent = "Enregistré";
    } catch (e) {
      if (status) status.textContent = "Non enregistré";
      toast(e.message, true);
    }
  };
  if (status) status.textContent = "…";
  if (now) return go();
  saveTimer = setTimeout(go, 500);
}
const changed = () => save(flow);

// ------------------------------------------------------------ en-tête
function bindHead(el) {
  const pick = $(".af-pick", el), name = $(".af-name", el);
  const fill = () => {
    pick.innerHTML = flows.map((f) => `<option value="${esc(f.id)}"${f === flow ? " selected" : ""}>${esc(f.name)}</option>`).join("");
    name.value = flow.name;
  };
  fill();
  pick.addEventListener("change", () => { flow = flows.find((f) => f.id === pick.value); remember(flow.id); sel = null; fill(); drawAll(el); fit(el); });
  name.addEventListener("input", () => {
    const v = name.value.trim();
    if (!v) return;
    flow.name = v;
    pick.selectedOptions[0].textContent = v;
    changed();
  });
  $("[data-new]", el).addEventListener("click", async () => {
    const f = newFlow(`Flow ${flows.length + 1}`);
    flows.push(f);
    flow = f; remember(f.id); sel = null; view = { x: 0, y: 0, k: 1 };
    await save(f, true);
    fill(); drawAll(el);
    name.select();
  });
  $("[data-del]", el).addEventListener("click", async () => {
    if (!(await confirmBox(`Supprimer le flow « ${flow.name} » ?\n\nSes nœuds et leurs réglages sont perdus.`, { ok: "Supprimer", danger: true }))) return;
    await api(`/api/automations/${flow.id}`, { method: "DELETE" }).catch((e) => toast(e.message, true));
    flows = flows.filter((f) => f !== flow);
    if (!flows.length) { flows = [newFlow("Mon premier flow")]; await save(flows[0], true); }
    flow = flows[0]; remember(flow.id); sel = null;
    fill(); drawAll(el);
  });
}

// ------------------------------------------------------------ palette
function bindPalette(el) {
  const canvas = $(".af-canvas", el);
  $$(".af-pal", el).forEach((p) => {
    p.addEventListener("dragstart", (e) => { e.dataTransfer.setData("application/x-drifty-node", p.dataset.kind); e.dataTransfer.effectAllowed = "copy"; });
    // Clic : à la suite du nœud choisi (sinon du plus à droite), sans recouvrir les autres.
    p.addEventListener("click", () => {
      const after = (sel?.node && nodeOf(flow, sel.node)) || flow.nodes.reduce((a, b) => (b.x > a.x ? b : a));
      let x = after.x + NODE_W + 60, y = after.y;
      while (flow.nodes.some((n) => Math.abs(n.x - x) < NODE_W && Math.abs(n.y - y) < 80)) y += 100;
      const fromSel = sel?.node;
      place(el, p.dataset.kind, x, y);
      // Ajouté à la suite du nœud choisi : relié à lui.
      if (fromSel && !connect(flow, fromSel, sel.node)) { changed(); drawEdges(el); }
    });
  });
  canvas.addEventListener("dragover", (e) => { if (e.dataTransfer.types.includes("application/x-drifty-node")) { e.preventDefault(); e.dataTransfer.dropEffect = "copy"; } });
  canvas.addEventListener("drop", (e) => {
    const kind = e.dataTransfer.getData("application/x-drifty-node");
    if (!kind) return;
    e.preventDefault();
    const at = toWorld(canvas, e.clientX, e.clientY);
    place(el, kind, at.x - NODE_W / 2, at.y - 24);
  });
}

function place(el, kind, x, y) {
  const n = addNode(flow, kind, x, y);
  sel = { node: n.id };
  changed();
  drawAll(el);
}

// ------------------------------------------------------------ plan
const toWorld = (canvas, cx, cy) => {
  const r = canvas.getBoundingClientRect();
  return { x: (cx - r.left - view.x) / view.k, y: (cy - r.top - view.y) / view.k };
};

function applyView(el) {
  $(".af-world", el).style.transform = `translate(${view.x}px, ${view.y}px) scale(${view.k})`;
  $("[data-zoom='0']", el).textContent = `${Math.round(view.k * 100)}%`;
  const c = $(".af-canvas", el);
  c.style.backgroundPosition = `${view.x}px ${view.y}px`;
  c.style.backgroundSize = `${22 * view.k}px ${22 * view.k}px`;
}

/** Cadre tout le flow dans la vue (sans grossir au-delà de 100 %). */
function fit(el) {
  const canvas = $(".af-canvas", el), r = canvas.getBoundingClientRect();
  if (!r.width || !flow.nodes.length) return;
  const xs = flow.nodes.map((n) => n.x), ys = flow.nodes.map((n) => n.y);
  const [x0, y0, x1, y1] = [Math.min(...xs), Math.min(...ys), Math.max(...xs) + NODE_W, Math.max(...ys) + 80];
  const pad = 60;
  view.k = Math.max(0.35, Math.min(1, (r.width - 2 * pad) / (x1 - x0), (r.height - 2 * pad) / (y1 - y0)));
  view.x = (r.width - (x1 - x0) * view.k) / 2 - x0 * view.k;
  view.y = (r.height - (y1 - y0) * view.k) / 2 - y0 * view.k;
  applyView(el);
}

function zoomAt(el, factor, cx, cy) {
  const canvas = $(".af-canvas", el), r = canvas.getBoundingClientRect();
  const k = Math.min(2, Math.max(0.35, view.k * factor));
  const px = cx - r.left, py = cy - r.top;
  view.x = px - ((px - view.x) / view.k) * k;
  view.y = py - ((py - view.y) / view.k) * k;
  view.k = k;
  applyView(el);
}

function bindCanvas(el) {
  const canvas = $(".af-canvas", el);
  canvas.addEventListener("wheel", (e) => {
    e.preventDefault();
    // Pincement / Ctrl+molette : zoom ; molette seule : déplacement (comme une page).
    if (e.ctrlKey || e.metaKey) zoomAt(el, Math.exp(-e.deltaY * 0.01), e.clientX, e.clientY);
    else { view.x -= e.deltaX; view.y -= e.deltaY; applyView(el); }
  }, { passive: false });
  $$("[data-zoom]", el).forEach((b) => b.addEventListener("click", () => {
    const r = canvas.getBoundingClientRect(), d = +b.dataset.zoom;
    if (d) zoomAt(el, d > 0 ? 1.2 : 1 / 1.2, r.left + r.width / 2, r.top + r.height / 2);
    else fit(el);
  }));

  canvas.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    const port = e.target.closest(".af-port.out");
    const node = e.target.closest(".af-node");
    const edge = e.target.closest(".af-hit");
    if (port) return startLink(el, e, port.closest(".af-node").dataset.id);
    if (edge) { sel = { edge: [edge.dataset.from, edge.dataset.to] }; drawAll(el); return; }
    if (node) return startMove(el, e, node);
    // Fond : déplacement du plan (et la sélection se retire).
    e.preventDefault(); // pas de sélection de texte pendant le glissé
    if (sel) { sel = null; drawAll(el); }
    const sx = e.clientX - view.x, sy = e.clientY - view.y;
    canvas.setPointerCapture(e.pointerId);
    canvas.classList.add("panning");
    const move = (m) => { view.x = m.clientX - sx; view.y = m.clientY - sy; applyView(el); };
    const up = () => { canvas.classList.remove("panning"); canvas.removeEventListener("pointermove", move); canvas.removeEventListener("pointerup", up); };
    canvas.addEventListener("pointermove", move);
    canvas.addEventListener("pointerup", up);
  });

  canvas.addEventListener("keydown", (e) => {
    if (e.target.closest("input, textarea, select")) return;
    if (e.key !== "Delete" && e.key !== "Backspace") return;
    e.preventDefault();
    deleteSelection(el);
  });
}

function deleteSelection(el) {
  if (sel?.node) {
    if (!removeNode(flow, sel.node)) return toast("Le Start reste : c'est le départ du flow.", true);
  } else if (sel?.edge) removeEdge(flow, ...sel.edge);
  else return;
  sel = null;
  changed();
  drawAll(el);
}

function startMove(el, e, nodeEl) {
  const n = nodeOf(flow, nodeEl.dataset.id);
  const canvas = $(".af-canvas", el);
  if (!sel || sel.node !== n.id) { sel = { node: n.id }; drawInspector(el); markSelection(el); }
  if (e.target.closest("input, textarea, select, button")) return;
  if (KINDS[n.type].fixed) return; // le Start ne bouge pas (pour l'instant)
  canvas.focus({ preventScroll: true });
  const start = { x: e.clientX, y: e.clientY, nx: n.x, ny: n.y };
  let moved = false;
  canvas.setPointerCapture(e.pointerId);
  const move = (m) => {
    n.x = Math.round(start.nx + (m.clientX - start.x) / view.k);
    n.y = Math.round(start.ny + (m.clientY - start.y) / view.k);
    nodeEl.style.left = n.x + "px";
    nodeEl.style.top = n.y + "px";
    moved = true;
    drawEdges(el);
  };
  const up = () => { canvas.removeEventListener("pointermove", move); canvas.removeEventListener("pointerup", up); if (moved) changed(); };
  canvas.addEventListener("pointermove", move);
  canvas.addEventListener("pointerup", up);
}

function startLink(el, e, from) {
  e.preventDefault();
  const canvas = $(".af-canvas", el), svg = $(".af-edges", el);
  const a = portPos(el, from, "out");
  const temp = document.createElementNS("http://www.w3.org/2000/svg", "path");
  temp.setAttribute("class", "af-edge temp");
  svg.append(temp);
  canvas.setPointerCapture(e.pointerId);
  canvas.classList.add("linking");
  const move = (m) => {
    const p = toWorld(canvas, m.clientX, m.clientY);
    temp.setAttribute("d", edgePath(a.x, a.y, p.x, p.y));
  };
  const up = (u) => {
    canvas.removeEventListener("pointermove", move);
    canvas.removeEventListener("pointerup", up);
    canvas.classList.remove("linking");
    temp.remove();
    // Lâché sur un nœud (n'importe où sur sa carte) : lien vers son entrée.
    const target = document.elementFromPoint(u.clientX, u.clientY)?.closest(".af-node");
    if (!target || target.dataset.id === from) return;
    const why = connect(flow, from, target.dataset.id);
    if (why) return toast(why, true);
    changed();
    drawEdges(el);
  };
  canvas.addEventListener("pointermove", move);
  canvas.addEventListener("pointerup", up);
  move(e);
}

// ------------------------------------------------------------ dessin
function drawAll(el) {
  drawNodes(el);
  drawEdges(el);
  drawInspector(el);
  applyView(el);
}

function drawNodes(el) {
  $(".af-nodes", el).innerHTML = flow.nodes.map((n) => {
    const k = KINDS[n.type];
    return `<div class="af-node${k.fixed ? " fixed" : ""}" data-id="${esc(n.id)}" style="left:${n.x}px;top:${n.y}px;--tone:${k.tone}">
      ${k.fixed ? "" : `<span class="af-port in" title="Entrée"></span>`}
      <div class="af-node-h">${icon(k.icon)}<b>${esc(k.label)}</b></div>
      <div class="af-node-s">${esc(k.summary(n.config || {}))}</div>
      <span class="af-port out" title="Tirer pour relier"></span>
    </div>`;
  }).join("");
  markSelection(el);
}

function markSelection(el) {
  $$(".af-node", el).forEach((n) => n.classList.toggle("on", sel?.node === n.dataset.id));
  $$(".af-edge:not(.temp)", el).forEach((p) => p.classList.toggle("on", !!sel?.edge && sel.edge[0] === p.dataset.from && sel.edge[1] === p.dataset.to));
}

function portPos(el, id, side) {
  const n = nodeOf(flow, id);
  const h = el.querySelector(`.af-node[data-id="${CSS.escape(id)}"]`)?.offsetHeight || 64;
  return { x: side === "out" ? n.x + NODE_W : n.x, y: n.y + h / 2 };
}

function drawEdges(el) {
  const svg = $(".af-edges", el);
  svg.innerHTML = flow.edges.map((e) => {
    const a = portPos(el, e.from, "out"), b = portPos(el, e.to, "in");
    const d = edgePath(a.x, a.y, b.x, b.y);
    const attrs = `data-from="${esc(e.from)}" data-to="${esc(e.to)}"`;
    return `<path class="af-hit" d="${d}" ${attrs}></path><path class="af-edge" d="${d}" ${attrs}></path>`;
  }).join("");
  markSelection(el);
}

// ------------------------------------------------------------ réglages du nœud choisi
const repoOptions = (v) => `<option value="">— Choisir un projet —</option>` + store.repos.filter((r) => !r.hidden).map((r) => `<option value="${r.id}"${+v === r.id ? " selected" : ""}>${esc(r.name)}</option>`).join("");
const opts = (list, v) => list.map(([k, l]) => `<option value="${esc(k)}"${k === v ? " selected" : ""}>${esc(l)}</option>`).join("");

const FORMS = {
  start: () => `<label>Déclenchement<select class="field" disabled><option>Manuel (bouton « Exécuter »)</option></select></label>
    <p class="muted small">Le départ du flow. Fixe pour l'instant ; d'autres déclencheurs (heure, webhook entrant, événement GitHub) viendront ensuite.</p>`,
  github: (c) => `<label>Projet<select class="field" data-k="repo_id" data-num>${repoOptions(c.repo_id)}</select></label>
    <label>Action<select class="field" data-k="action"><option value="">À définir ensemble</option></select></label>
    <p class="muted small">Les actions GitHub (PR, CI, relectures, issues…) seront choisies avec toi.</p>`,
  ai: (c) => `<label>Projet<select class="field" data-k="repo_id" data-num>${repoOptions(c.repo_id)}</select></label>
    <div class="af-row"><label>Compte<select class="field" data-k="slot">${opts([["perso", "Perso"], ["pro", "Pro"]], c.slot)}</select></label>
    <label>Modèle<select class="field" data-k="model">${opts([["haiku", "Haiku"], ["sonnet", "Sonnet"], ["opus", "Opus"]], c.model)}</select></label></div>
    <label>Prompt<textarea class="field area" rows="8" data-k="prompt" placeholder="Ce que l'agent doit faire…">${esc(c.prompt || "")}</textarea></label>`,
  bash: (c) => `<label>Dossier (projet)<select class="field" data-k="repo_id" data-num>${repoOptions(c.repo_id)}</select></label>
    <label>Commande<textarea class="field area mono" rows="6" data-k="command" spellcheck="false" placeholder="make test">${esc(c.command || "")}</textarea></label>
    <label>Délai maximal (s)<input class="field" type="number" min="1" max="86400" data-k="timeout_s" data-num value="${esc(c.timeout_s ?? 600)}"></label>`,
  webhook: (c) => `<div class="af-row"><label style="flex:0 0 110px">Méthode<select class="field" data-k="method">${opts([["POST", "POST"], ["PUT", "PUT"], ["PATCH", "PATCH"], ["GET", "GET"]], c.method)}</select></label>
    <label>URL<input class="field mono" data-k="url" spellcheck="false" placeholder="https://…" value="${esc(c.url || "")}"></label></div>
    <label>En-têtes<textarea class="field area mono" rows="3" data-k="headers" spellcheck="false" placeholder="Authorization: Bearer …">${esc(c.headers || "")}</textarea></label>
    <label>Corps<textarea class="field area mono" rows="6" data-k="body" spellcheck="false" placeholder='{"texte": "…"}'>${esc(c.body || "")}</textarea></label>`,
};

function drawInspector(el) {
  const box = $(".af-inspect", el);
  if (sel?.edge) {
    const [a, b] = sel.edge.map((id) => KINDS[nodeOf(flow, id)?.type]?.label || id);
    box.innerHTML = `<div class="af-ins-t">Lien</div><p>${esc(a)} → ${esc(b)}</p><button class="btn" type="button" data-rm>Retirer le lien</button>`;
    $("[data-rm]", box).addEventListener("click", () => deleteSelection(el));
    return;
  }
  const n = sel?.node && nodeOf(flow, sel.node);
  if (!n) {
    box.innerHTML = `<div class="af-ins-t">Réglages</div><p class="muted small">Choisis un nœud pour le régler.</p>
      <p class="muted small">${flow.nodes.length - 1} nœud${flow.nodes.length > 2 ? "s" : ""} · ${flow.edges.length} lien${flow.edges.length > 1 ? "s" : ""}</p>`;
    return;
  }
  const k = KINDS[n.type];
  box.innerHTML = `<div class="af-ins-t" style="--tone:${k.tone}">${icon(k.icon)}${esc(k.label)}</div>
    <form class="af-form" onsubmit="return false">${FORMS[n.type](n.config || {})}</form>
    ${k.fixed ? "" : `<button class="btn" type="button" data-rm>Supprimer le nœud</button>`}`;
  $("[data-rm]", box)?.addEventListener("click", () => deleteSelection(el));
  $(".af-form", box).addEventListener("input", (e) => {
    const f = e.target.closest("[data-k]");
    if (!f) return;
    n.config[f.dataset.k] = f.hasAttribute("data-num") ? (f.value === "" ? null : +f.value) : f.value;
    // Aperçu sur la carte, sans redessiner le plan (le champ garde la main).
    const s = el.querySelector(`.af-node[data-id="${CSS.escape(n.id)}"] .af-node-s`);
    if (s) s.textContent = k.summary(n.config);
    drawEdges(el);
    changed();
  });
}
