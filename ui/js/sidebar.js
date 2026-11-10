// Barre latérale (inspirée d'Orca) : recherche, navigation, arbre projets →
// worktrees → conversations, et jauges d'usage en pied. Persistante : seule la
// zone principale change d'une page à l'autre.
import { api, esc, ago, icon, logo, badges, toast, $, $$ } from "./lib.js";
import { store, subscribe, ensureLoaded, convsFor } from "./store.js";
import { newChatDialog } from "./views/chat.js";
import { editProject } from "./views/project.js";

const PER_GROUP = 5;
const ui = { query: "", expanded: new Set(), open: new Set() };
try {
  const saved = JSON.parse(localStorage.getItem("claudius.tree") || "{}");
  (saved.expanded || []).forEach((id) => ui.expanded.add(id));
} catch {}
const saveTree = () => { try { localStorage.setItem("claudius.tree", JSON.stringify({ expanded: [...ui.expanded] })); } catch {} };

/** `claude-opus-5-5` → `opus-5.5` (comme dans Orca). */
export function modelShort(id) {
  if (!id) return "";
  const m = /^claude-([a-z]+)-(\d+)(?:-(\d{1,2}))?(?:-\d{8})?$/.exec(id);
  return m ? `${m[1]}-${m[2]}${m[3] ? "." + m[3] : ""}` : id;
}

const age = (ts) => {
  if (!ts) return "";
  const s = (Date.now() - ts) / 1000;
  if (s < 60) return "now";
  if (s < 3600) return Math.floor(s / 60) + "m";
  if (s < 86400) return Math.floor(s / 3600) + "h";
  return Math.floor(s / 86400) + "j";
};

function selection() {
  const q = new URLSearchParams(location.hash.split("?")[1] || "");
  return { s: q.get("s"), c: q.get("c"), page: (location.hash.slice(2).split("?")[0] || "usage") };
}

// ------------------------------------------------------------ statut
const STATUS = {
  working: `<span class="st st-run" title="En cours"></span>`,
  unread: `<span class="st st-unread" title="Terminée, non lue"></span>`,
  idle: `<span class="st st-idle" title="En attente"></span>`,
  done: `<span class="st st-done" title="Terminée">${icon("check")}</span>`,
};
const liveStatus = (chat) => (!chat ? "done" : chat.status === "working" ? "working" : chat.unread ? "unread" : chat.status === "exited" ? "done" : "idle");

function convRow(c, chat, sel) {
  const on = (chat && chat.id === sel.c) || c.id === sel.s;
  const href = chat ? `#/conversations?c=${chat.id}` : `#/conversations?s=${c.id}`;
  const model = modelShort(chat?.model || c.model);
  return `<a class="sb-conv${on ? " on" : ""}" href="${href}" title="${esc(c.title || c.first_prompt || "")}">
    ${STATUS[liveStatus(chat)]}<span class="sb-title">${esc(c.title || c.first_prompt || c.id.slice(0, 8))}</span>
    <span class="sb-meta">${esc(model)} ${age(chat ? Date.now() : c.last_ts)}</span>
  </a>`;
}

/** Discussions lancées dans l'app qui ne sont pas encore dans la liste (nouvelles). */
function newChatRow(chat, sel) {
  return convRow({ id: chat.session_id || chat.id, title: chat.name, last_ts: chat.created }, chat, sel);
}

// ------------------------------------------------------------ arbre
async function projectHtml(repo, sel) {
  const open = ui.expanded.has(repo.id);
  const chats = store.chats.filter((c) => c.repo_id === repo.id);
  const k = { active: chats.filter((c) => c.status === "working").length, unread: chats.filter((c) => c.unread).length };
  let body = "";
  if (open) {
    const convs = await convsFor(repo.id);
    const bySession = new Map(chats.filter((c) => c.session_id).map((c) => [c.session_id, c]));
    const listed = new Set(convs.map((c) => c.id));
    const groups = repo.groups.length ? repo.groups : [{ worktree: "", rel: "" }];
    const single = groups.length === 1;
    body = groups.map((g) => {
      const inGroup = convs.filter((c) => c.worktree === g.worktree && c.rel === g.rel);
      const fresh = chats.filter((c) => !(c.session_id && listed.has(c.session_id)) && (c.worktree || "") === g.worktree && !g.rel);
      const key = `${repo.id}|${g.worktree}|${g.rel}`;
      const all = ui.open.has(key);
      const shown = all ? inGroup : inGroup.slice(0, PER_GROUP);
      const rows = fresh.map((c) => newChatRow(c, sel)).join("") + shown.map((c) => convRow(c, bySession.get(c.id), sel)).join("");
      const more = inGroup.length > PER_GROUP ? `<button class="sb-more" data-more="${esc(key)}">${all ? "Moins" : `${inGroup.length - PER_GROUP} de plus`}</button>` : "";
      if (single) return rows + more;
      const working = fresh.concat(inGroup.map((c) => bySession.get(c.id)).filter(Boolean)).some((c) => c.status === "working");
      const branch = inGroup.find((c) => c.branch)?.branch || "";
      const name = g.worktree || (g.rel ? g.rel : repo.root.split("/").pop());
      return `<div class="sb-wt">
        <div class="sb-wt-row">${working ? STATUS.working : `<span class="st st-wt"></span>`}<span class="sb-wt-name">${esc(g.rel && g.worktree ? `${g.worktree}/${g.rel}` : name)}</span>${!g.worktree && !g.rel ? `<span class="sb-tag">primary</span>` : ""}</div>
        ${branch ? `<div class="sb-branch">${esc(branch)}</div>` : ""}
        ${rows}${more}
      </div>`;
    }).join("");
  }
  return `<div class="sb-proj${open ? " open" : ""}">
    <div class="sb-proj-row" data-repo="${repo.id}" title="${esc(repo.root)}">
      <span class="sb-chev">${icon("chevron")}</span>
      <span class="logo-wrap">${logo(repo, "sm")}${badges(k.active, k.unread)}</span>
      <span class="sb-proj-name">${esc(repo.name)}</span>
      <span class="sb-acts">
        <button class="sb-act" data-new="${repo.id}" title="Nouvelle discussion">${icon("plus")}</button>
        <button class="sb-act" data-edit="${repo.id}" title="Réglages du projet">${icon("gear")}</button>
      </span>
    </div>
    ${body}
  </div>`;
}

// Extrait renvoyé par la recherche : \u0001 … \u0002 entourent les mots trouvés.
const snippet = (t) => esc(t).replace(/\u0001/g, "<mark>").replace(/\u0002/g, "</mark>");

async function searchHtml(sel) {
  const q = ui.query.toLowerCase();
  const content = await api(`/api/search?q=${encodeURIComponent(ui.query)}`).catch(() => []);
  const visible = store.repos.filter((r) => !r.hidden);
  const lists = await Promise.all(visible.map((r) => convsFor(r.id).then((cs) => cs.map((c) => ({ ...c, repo: r })))));
  const hits = lists.flat().filter((c) => `${c.title || ""} ${c.first_prompt || ""}`.toLowerCase().includes(q)).slice(0, 60);
  const titled = new Set(hits.map((c) => c.id));
  const inside = content.filter((h) => !titled.has(h.session_id));
  if (!hits.length && !inside.length) return `<div class="sb-empty">Aucun résultat</div>`;
  const bySession = new Map(store.chats.filter((c) => c.session_id).map((c) => [c.session_id, c]));
  const repoOf = (id) => store.repos.find((r) => r.id === id) || { name: "?", key: "?" };
  return (hits.length ? `<div class="sb-group">Titres</div>` + hits.map((c) => `<div class="sb-hit">${logo(c.repo, "xs")}${convRow(c, bySession.get(c.id), sel)}</div>`).join("") : "") +
    (inside.length ? `<div class="sb-group">Dans les conversations</div>` + inside.map((h) => `
      <a class="sb-found${h.session_id === sel.s ? " on" : ""}" href="#/conversations?s=${h.session_id}">
        <span class="sb-found-top">${logo(repoOf(h.repo_id), "xs")}<span class="sb-title">${esc(h.title || h.session_id.slice(0, 8))}</span><span class="sb-meta">${h.hits > 1 ? h.hits + "×" : ""} ${age(h.ts)}</span></span>
        <span class="sb-snip">${h.role === "user" ? "Toi : " : ""}${snippet(h.snippet)}</span>
      </a>`).join("") : "");
}

let drawing = 0;
async function drawTree() {
  const box = $("#sb-tree");
  if (!box) return;
  const sel = selection();
  // Le projet de la conversation affichée est toujours déplié.
  const id = ++drawing;
  const html = ui.query
    ? await searchHtml(sel)
    : (await Promise.all(store.repos.filter((r) => !r.hidden).map((r) => projectHtml(r, sel)))).join("") ||
      `<div class="sb-empty">Aucun projet. Lance une analyse.</div>`;
  if (id !== drawing) return; // un dessin plus récent a pris le relais
  const top = box.scrollTop;
  box.innerHTML = html;
  box.scrollTop = top;
}

function drawNav() {
  const page = selection().page;
  $$(".sb-nav a").forEach((a) => a.classList.toggle("on", a.dataset.nav === page));
}

// ------------------------------------------------------------ jauges d'usage en pied
function resetIn(ms) {
  if (!ms) return "";
  const s = Math.max(0, (ms - Date.now()) / 1000);
  if (s < 3600) return `${Math.round(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ${String(Math.round((s % 3600) / 60)).padStart(2, "0")}m`;
  return `${Math.floor(s / 86400)}d ${Math.floor((s % 86400) / 3600)}h`;
}
async function drawUsage() {
  const box = $("#sb-usage");
  if (!box) return;
  const accs = await api("/api/usage").catch(() => []);
  const w = (x) => (x && x.used_percentage != null ? x : null);
  box.innerHTML = accs.filter((a) => a.enabled).map((a) => {
    const h = w(a.five_hour), d = w(a.seven_day);
    const pct = h ? Math.round(h.used_percentage) : null;
    return `<a class="sb-u ${a.slot}" href="#/" title="${esc(a.label)} : session 5 h${h ? ` ${pct} %` : ""}, semaine${d ? ` ${Math.round(d.used_percentage)} %` : ""}">
      <span class="sb-u-dot"></span><span class="sb-u-track"><span style="width:${pct ?? 0}%"></span></span>
      <span>${h ? `${pct}% ${resetIn(h.resets_at_ms)}` : "—"}${d ? ` · ${Math.round(d.used_percentage)}% ${resetIn(d.resets_at_ms)}` : ""}</span>
    </a>`;
  }).join("");
}

// ------------------------------------------------------------ montage
export async function mountSidebar(el) {
  el.innerHTML = `
    <div class="sb-head" data-tauri-drag-region="deep"><span class="sb-brand">${document.getElementById("mark-tpl")?.innerHTML || ""}<span class="brand-word">DRIFT<i>y</i></span></span>
      <button class="icon-btn" id="sb-toggle" title="Masquer la barre">${icon("sidebar")}</button></div>
    <label class="sb-search">${icon("search")}<input id="sb-q" placeholder="Rechercher" autocomplete="off"></label>
    <nav class="sb-nav">
      <a href="#/" data-nav="usage">${icon("gauge")}Usage</a>
      <a href="#/ledger" data-nav="ledger">${icon("scale")}Bilan</a>
      <button type="button" data-sync class="sb-sync">${icon("sync")}Analyser</button>
    </nav>
    <div class="sb-section">Projets</div>
    <div class="sb-tree" id="sb-tree"></div>
    <div class="sb-foot">
      <a class="icon-btn" href="#/accounts" title="Réglages">${icon("gear")}</a>
      <span style="flex:1"></span>
    </div>
    <div class="sb-usage" id="sb-usage"></div>`;
  await ensureLoaded();
  // Le projet de la conversation affichée se déplie tout seul.
  const sel = selection();
  if (!ui.expanded.size && store.repos[0]) ui.expanded.add(store.repos[0].id);
  if (sel.c) { const ch = store.chats.find((x) => x.id === sel.c); if (ch) ui.expanded.add(ch.repo_id); }

  let searchTimer;
  $("#sb-q", el).addEventListener("input", (e) => {
    ui.query = e.target.value.trim();
    clearTimeout(searchTimer);
    searchTimer = setTimeout(drawTree, ui.query ? 220 : 0);
  });
  $("#sb-toggle", el).addEventListener("click", () => document.body.classList.toggle("sb-hidden"));
  el.addEventListener("click", (e) => {
    const newBtn = e.target.closest("[data-new]");
    const editBtn = e.target.closest("[data-edit]");
    const more = e.target.closest("[data-more]");
    const row = e.target.closest(".sb-proj-row");
    if (newBtn) {
      e.stopPropagation();
      const repo = store.repos.find((r) => r.id === +newBtn.dataset.new);
      newChatDialog(repo, (chat) => { location.hash = `#/conversations?c=${chat.id}`; });
    } else if (editBtn) {
      e.stopPropagation();
      editProject(store.repos.find((r) => r.id === +editBtn.dataset.edit));
    } else if (more) {
      const k = more.dataset.more;
      ui.open.has(k) ? ui.open.delete(k) : ui.open.add(k);
      drawTree();
    } else if (row) {
      const id = +row.dataset.repo;
      ui.expanded.has(id) ? ui.expanded.delete(id) : ui.expanded.add(id);
      saveTree();
      drawTree();
    }
  });
  subscribe(drawTree);
  addEventListener("hashchange", () => { drawNav(); drawTree(); });
  drawNav();
  drawTree();
  drawUsage();
  setInterval(() => !document.hidden && drawUsage(), 5000); // relevé local écrit par le suivi, aucune synchronisation
}
