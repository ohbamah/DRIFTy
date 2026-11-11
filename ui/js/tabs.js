// Onglets ajoutés à une conversation, à côté de « Conversation » et « Analyse » : un
// terminal intégré (shell dans le dossier de la conversation, avec le port du worktree)
// ou un navigateur (une page, le plus souvent le serveur de dev du worktree). Ils restent
// ouverts quand on change d'onglet ou de conversation, jusqu'à ce qu'on les ferme.
import { api, esc, icon, toast, getTheme } from "./lib.js";
import { moveTo } from "./slide.js";
import { token, withElements } from "./elements.js";

const reg = new Map(); // conversation → { tabs: [...], active: id | null }
let seq = 0;

// Les panneaux (cadre du navigateur, terminal) ne sont JAMAIS déplacés dans le DOM : un
// <iframe> déplacé se recharge. Ils vivent dans un calque fixe posé sur la zone de
// l'onglet affiché ; changer d'onglet ou de conversation ne fait que les montrer / cacher.
const layer = document.createElement("div");
layer.className = "xtab-layer";
layer.hidden = true;
document.body.append(layer);
let shown = null; // { host, el }
function place() {
  if (!shown) return;
  const { host } = shown;
  if (!host.isConnected || !host.closest(".x-on")) { layer.hidden = true; shown = null; return; }
  const r = host.getBoundingClientRect();
  Object.assign(layer.style, { left: r.left + "px", top: r.top + "px", width: r.width + "px", height: r.height + "px" });
  layer.hidden = r.width === 0;
}
const ro = new ResizeObserver(place);
addEventListener("resize", place);
new MutationObserver(() => { if (shown && !shown.host.isConnected) place(); }).observe(document.body, { childList: true, subtree: true });
function showPanel(host, el) {
  if (!el.isConnected) layer.append(el);
  for (const c of layer.children) c.hidden = c !== el;
  if (shown?.host !== host) { if (shown) ro.unobserve(shown.host); ro.observe(host); }
  shown = { host, el };
  place();
}
function hidePanel() {
  if (shown) ro.unobserve(shown.host);
  shown = null;
  layer.hidden = true;
}

// Battement : les relais des onglets navigateur encore ouverts restent en vie côté serveur
// (un relais sans activité ni page qui le montre est arrêté au bout de 3 min).
setInterval(() => {
  const relays = [...reg.values()].flatMap((st) => st.tabs.filter((t) => t.kind === "web" && t.relay).map((t) => t.relay));
  if (relays.length) api("/api/web/alive", { method: "POST", json: { relays: [...new Set(relays)] } }).catch(() => {});
}, 60_000);

// ------------------------------------------------------------ xterm.js, chargé à la demande
let xtermReady = null;
function loadXterm() {
  if (xtermReady) return xtermReady;
  const css = document.createElement("link");
  css.rel = "stylesheet";
  css.href = "/ui/vendor/xterm/xterm.css";
  document.head.append(css);
  const script = (src) => new Promise((ok, ko) => { const s = document.createElement("script"); s.src = src; s.onload = ok; s.onerror = () => ko(new Error(`${src} introuvable`)); document.head.append(s); });
  xtermReady = script("/ui/vendor/xterm/xterm.js").then(() => Promise.all([script("/ui/vendor/xterm/addon-fit.js"), script("/ui/vendor/xterm/addon-web-links.js")]));
  return xtermReady;
}
const dark = () => (getTheme() === "dark" || (getTheme() === "system" && matchMedia("(prefers-color-scheme: dark)").matches));
const termTheme = () => (dark()
  ? { background: "#0d1117", foreground: "#e6edf3", cursor: "#e6edf3", selectionBackground: "#264f78" }
  : { background: "#ffffff", foreground: "#1f2328", cursor: "#1f2328", selectionBackground: "#b6d7ff" });

// ------------------------------------------------------------ fabrication des onglets
async function makeTerminal(target) {
  await loadXterm();
  const el = document.createElement("div");
  el.className = "xterm-pane";
  const t = new window.Terminal({ fontFamily: '"MesloLGS NF", "JetBrainsMono Nerd Font", "Hack Nerd Font", "FiraCode Nerd Font", "Symbols Nerd Font Mono", ui-monospace, "SF Mono", Menlo, monospace', fontSize: 12.5, cursorBlink: true, scrollback: 5000, theme: termTheme(), allowProposedApi: true });
  const fit = new window.FitAddon.FitAddon();
  t.loadAddon(fit);
  t.loadAddon(new window.WebLinksAddon.WebLinksAddon((e, uri) => window.open(uri, "_blank", "noopener")));
  const r = await api("/api/term", { method: "POST", json: { ...target, cols: 120, rows: 32 } });
  const ws = new WebSocket(`${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/api/term/${r.id}/ws`);
  ws.binaryType = "arraybuffer";
  let ended = false;
  ws.onmessage = (m) => t.write(new Uint8Array(m.data));
  ws.onclose = () => { if (!ended) { ended = true; t.write("\r\n\x1b[2m[shell terminé]\x1b[0m\r\n"); } };
  t.onData((d) => ws.readyState === 1 && ws.send(JSON.stringify({ t: "i", d })));
  t.onResize(({ cols, rows }) => ws.readyState === 1 && ws.send(JSON.stringify({ t: "r", c: cols, r: rows })));
  ws.onopen = () => { try { fit.fit(); } catch {} ws.send(JSON.stringify({ t: "r", c: t.cols, r: t.rows })); };
  let opened = false;
  const ro = new ResizeObserver(() => { if (el.offsetParent) { if (!opened) { t.open(el); opened = true; } try { fit.fit(); } catch {} } });
  ro.observe(el);
  return {
    kind: "term", el, title: "Terminal",
    focus: () => setTimeout(() => t.focus(), 30),
    dispose: () => { ended = true; ro.disconnect(); ws.close(); t.dispose(); api(`/api/term/${r.id}`, { method: "DELETE" }).catch(() => {}); },
  };
}

async function makeBrowser(target, deliver, { url = "", openTab = null } = {}) {
  const el = document.createElement("div");
  el.className = "web-pane";
  el.innerHTML = `<div class="web-bar">
      <button class="icon-btn" data-back title="Page précédente">${icon("back")}</button>
      <button class="icon-btn" data-fwd title="Page suivante">${icon("chevron")}</button>
      <button class="icon-btn" data-reload title="Recharger">${icon("sync")}</button>
      <form class="web-url"><input class="field" placeholder="Adresse : localhost:3000, app.exemple.io…" spellcheck="false"></form>
      <button class="icon-btn web-pick" data-pick title="Choisir des éléments de la page pour un prompt (Maj+clic : plusieurs)">${icon("cursor")}</button>
      <button class="icon-btn" data-ext title="Ouvrir dans le navigateur">${icon("forward")}</button>
    </div>
    <div class="web-sel" hidden>
      <div class="web-chips"></div>
      <div class="web-ask"><textarea class="field" rows="1" placeholder="Que faire avec cette sélection ?"></textarea>
        ${deliver.send ? `<button class="btn primary small" data-send>Envoyer à Claude</button>` : ""}
        <button class="btn small" data-draft>Joindre à la saisie</button></div>
    </div>
    <iframe class="web-frame" referrerpolicy="no-referrer-when-downgrade" allow="clipboard-read; clipboard-write; fullscreen"></iframe>
    <div class="web-empty">Tape l'adresse d'une page. Elle passe par un relais local qui l'autorise à s'afficher ici, et permet d'en choisir des éléments (flèche) pour en faire un prompt.</div>`;
  const $q = (s) => el.querySelector(s);
  const input = $q("input"), frame = $q("iframe"), empty = $q(".web-empty"), sel = $q(".web-sel"), chips = $q(".web-chips"), ask = $q(".web-ask textarea"), pickBtn = $q("[data-pick]");
  const tab = { kind: "web", el, title: "Navigateur", url: "", focus: () => {}, dispose: () => { removeEventListener("message", onMsg); frame.src = "about:blank"; } };
  let picked = [], picking = false;
  const post = (m) => frame.contentWindow?.postMessage({ drifty: 1, ...m }, "*");
  const setPick = (on) => { picking = on; pickBtn.classList.toggle("on", on); post({ cmd: "pick", on }); };
  const drawSel = () => {
    sel.hidden = !picked.length;
    chips.innerHTML = picked.map((e, i) => `<span class="web-chip" title="${esc(e.selector)}"><code>${esc(e.short)}</code>${e.text ? `<span>${esc(e.text.slice(0, 40))}</span>` : ""}<button data-rm="${i}" title="Retirer">${icon("close")}</button></span>`).join("")
      + `<button class="btn small" data-clear>Tout retirer</button>`;
    chips.querySelectorAll("[data-rm]").forEach((b) => b.addEventListener("click", () => { picked.splice(+b.dataset.rm, 1); drawSel(); }));
    chips.querySelector("[data-clear]").addEventListener("click", () => { picked = []; drawSel(); });
  };
  // Messages du script injecté dans la page : seulement ceux de CE cadre.
  const onMsg = (e) => {
    if (e.source !== frame.contentWindow || !e.data?.drifty) return;
    const m = e.data;
    if (m.type === "nav") {
      tab.url = m.url;
      tab.relay = m.relay;
      if (document.activeElement !== input) input.value = m.url;
      try { tab.title = m.title?.trim() || new URL(m.url).host; } catch {}
      tab.rename?.(tab.title.slice(0, 40));
      if (picking) post({ cmd: "pick", on: true }); // nouvelle page : le sélecteur reste actif
    } else if (m.type === "picked") {
      picked = m.add ? [...picked, m.info] : [...picked.filter((p) => p.selector !== m.info.selector), m.info];
      if (!m.add) setPick(false);
      drawSel();
      ask.focus();
    } else if (m.type === "open" && m.url) {
      openTab ? openTab(m.url) : window.open(m.url, "_blank", "noopener");
    } else if (m.type === "pick-off") {
      picking = false;
      pickBtn.classList.remove("on");
    }
  };
  addEventListener("message", onMsg);
  const go = async (url) => {
    if (!url.trim()) return;
    try {
      const r = await api("/api/web", { method: "POST", json: { url } });
      tab.url = r.url;
      tab.relay = r.relay;
      input.value = r.url;
      input.blur(); // l'adresse réelle (après redirections) s'affichera au chargement
      frame.src = r.src;
      empty.hidden = true;
      frame.hidden = false;
      try { tab.title = new URL(r.url).host; } catch {}
      tab.rename?.(tab.title);
    } catch (e) { toast(e.message, true); }
  };
  // Envoi direct (chat en direct) : la demande, les repères, puis les blocs des éléments.
  // Sinon : les éléments sont JOINTS à la saisie de la conversation, comme des images.
  const finish = (how) => {
    if (!picked.length) return;
    if (how === "send") {
      const els = picked.map((info, i) => ({ n: i + 1, info }));
      deliver.send(withElements(`${ask.value.trim()} ${els.map((e) => token(e.n)).join(" ")}`.trim(), els));
      toast("Envoyé à Claude.");
    } else {
      const comp = document.querySelector("#detail .composer");
      if (!comp?.attachElement) return toast("La saisie de la conversation est introuvable.", true);
      if (ask.value.trim()) comp.addText(ask.value.trim());
      picked.forEach((info) => comp.attachElement(info));
      toast(`${picked.length > 1 ? `${picked.length} éléments joints` : "Élément joint"} à la saisie de la conversation.`);
    }
    picked = []; ask.value = ""; drawSel();
  };
  $q("form").addEventListener("submit", (e) => { e.preventDefault(); go(input.value); });
  $q("[data-reload]").addEventListener("click", () => { if (tab.url) go(tab.url); });
  $q("[data-back]").addEventListener("click", () => post({ cmd: "back" }));
  $q("[data-fwd]").addEventListener("click", () => post({ cmd: "forward" }));
  $q("[data-ext]").addEventListener("click", () => { if (tab.url) window.open(tab.url, "_blank", "noopener"); });
  pickBtn.addEventListener("click", () => { if (!tab.url) return toast("Ouvre d'abord une page.", true); setPick(!picking); });
  $q("[data-send]")?.addEventListener("click", () => finish("send"));
  $q("[data-draft]").addEventListener("click", () => finish("draft"));
  ask.addEventListener("keydown", (e) => { if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); finish(deliver.send ? "send" : "draft"); } });
  frame.hidden = true;
  // L'adresse demandée (nouvel onglet ouvert par une page), sinon le serveur du worktree.
  if (url) go(url);
  else api(`/api/web/default?${new URLSearchParams(target)}`).then((d) => { if (d.url) go(d.url); else setTimeout(() => input.focus(), 30); }).catch(() => {});
  tab.focus = () => { if (!tab.url) setTimeout(() => input.focus(), 30); };
  return tab;
}

// ------------------------------------------------------------ barre d'onglets
/**
 * key : la conversation (`s:<id>` ou `c:<id>`) ; target : `{session}` ou `{chat}` ;
 * tabsEl : la barre `.tabs` de l'en-tête ; detail : le conteneur `#detail` ;
 * deliver : `{ send(text)? }` : envoi direct à l'agent (chat en direct) ; sinon les éléments
 * choisis sont joints à la saisie de la conversation.
 */
export function mountTabs(detail, tabsEl, key, target, deliver = {}) {
  const st = reg.get(key) || { tabs: [], active: null };
  reg.set(key, st);
  // `detail` est réutilisé d'une conversation à l'autre : l'état « onglet ajouté affiché » de la
  // précédente (classe `x-on`, qui masque le corps) ne doit pas passer à celle-ci. Sinon son fil
  // restait invisible sous un « Conversation » pourtant actif. L'onglet actif propre à cette
  // conversation est rétabli plus bas.
  detail.classList.remove("x-on");
  hidePanel();
  const host = document.createElement("div");
  host.className = "xtab-host";
  detail.append(host);
  let builtin = tabsEl.querySelector(".tab.on[data-tab]");

  const plus = document.createElement("button");
  plus.className = "tab tab-plus";
  plus.type = "button";
  plus.title = "Ouvrir un terminal ou un navigateur";
  plus.innerHTML = icon("plus");
  tabsEl.append(plus);

  const button = (tab) => {
    const b = document.createElement("button");
    b.className = "tab xtab";
    b.type = "button";
    b.innerHTML = `<span class="xtab-ic">${tab.kind === "term" ? icon("terminal") : icon("search")}</span><span class="xtab-t">${esc(tab.title)}</span><span class="xtab-x" title="Fermer">${icon("close")}</span>`;
    b.addEventListener("click", (e) => {
      if (e.target.closest(".xtab-x")) return close(tab);
      show(tab);
    });
    tab.rename = (t) => { b.querySelector(".xtab-t").textContent = t; };
    tab.btn = b;
    tabsEl.insertBefore(b, plus);
  };
  const show = (tab) => {
    if (!detail.classList.contains("x-on")) builtin = tabsEl.querySelector(".tab.on[data-tab]") || builtin;
    st.active = tab.id;
    detail.classList.add("x-on");
    showPanel(host, tab.el);
    moveTo(tab.btn);
    tab.focus();
  };
  const hide = () => {
    st.active = null;
    detail.classList.remove("x-on");
    hidePanel();
  };
  const close = (tab) => {
    tab.dispose();
    tab.el.remove();
    tab.btn.remove();
    st.tabs.splice(st.tabs.indexOf(tab), 1);
    if (st.active === tab.id) {
      hide();
      if (builtin) moveTo(builtin);
    }
  };
  // Retour à « Conversation » / « Analyse » : l'onglet ajouté s'efface ; s'il s'agit de
  // l'onglet déjà affiché dessous, rien d'autre à faire (pas de rechargement).
  tabsEl.addEventListener("click", (e) => {
    const b = e.target.closest(".tab[data-tab]");
    if (!b || !detail.classList.contains("x-on")) return;
    hide();
    if (b === builtin) {
      moveTo(b);
      e.stopImmediatePropagation();
    }
  }, true);

  plus.addEventListener("click", () => {
    document.querySelector(".tab-menu")?.remove();
    const m = document.createElement("div");
    m.className = "ctx-pop tab-menu";
    m.innerHTML = `<button data-k="term">${icon("terminal")}Terminal</button><button data-k="web">${icon("search")}Navigateur</button>`;
    document.body.append(m);
    const r = plus.getBoundingClientRect();
    m.style.left = r.left + "px";
    m.style.top = r.bottom + 6 + "px";
    const off = (e) => { if (!m.contains(e.target) && e.target !== plus) { m.remove(); removeEventListener("mousedown", off); } };
    addEventListener("mousedown", off);
    m.querySelectorAll("button").forEach((x) => x.addEventListener("click", () => {
      m.remove();
      removeEventListener("mousedown", off);
      add(x.dataset.k);
    }));
  });

  // Un seul chemin de création : le menu « + », ou une page qui demande un nouvel onglet.
  async function add(kind, url = "") {
    try {
      const tab = await (kind === "term" ? makeTerminal(target) : makeBrowser(target, deliver, { url, openTab: (u) => add("web", u) }));
      tab.id = ++seq;
      const same = st.tabs.filter((t) => t.kind === tab.kind).length;
      if (tab.kind === "term" && same) tab.title = `Terminal ${same + 1}`;
      st.tabs.push(tab);
      // `detail` sert à toutes les conversations : c'est la barre d'onglets (propre à celle-ci)
      // qui dit si on y est encore. Sinon l'onglet, créé après un changement de conversation,
      // s'affichait par-dessus la suivante et masquait son fil.
      if (tabsEl.isConnected) { button(tab); show(tab); }
    } catch (e) { toast(e.message, true); }
  }

  for (const tab of st.tabs) button(tab);
  const active = st.tabs.find((t) => t.id === st.active);
  if (active) requestAnimationFrame(() => { if (tabsEl.isConnected) show(active); });
}
