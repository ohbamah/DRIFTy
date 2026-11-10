// Raccourcis clavier de DRIFTy. ⌘ sur macOS (Ctrl ailleurs). Dans Chrome, ⌘N, ⌘T et
// ⌘⇧T sont gardés par le navigateur ; dans l'app DRIFTy, tous marchent.
import { api, esc, $, $$ } from "./lib.js";
import { store } from "./store.js";
import { newChatDialog } from "./views/chat.js";

const SHORTCUTS = [
  ["⌘K", "Rechercher une conversation"],
  ["⌘N", "Nouvelle discussion (projet affiché)"],
  ["⌘1 … ⌘9", "Ouvrir la 1re … 9e conversation de la barre"],
  ["⌥↑  ⌥↓", "Conversation précédente / suivante"],
  ["⌘L", "Aller à la saisie"],
  ["⌘B", "Afficher / masquer la barre latérale"],
  ["⌘T", "Nouvel onglet terminal"],
  ["⌘⇧T", "Nouvel onglet navigateur"],
  ["⌘⇧W", "Fermer l'onglet terminal / navigateur affiché"],
  ["⌘⇧G", "Panneau GitHub"],
  ["⌘⇧D", "Changements du dossier"],
  ["⌘/", "Cette aide"],
];

function help() {
  if ($("dialog.keys-dlg")) return;
  const d = document.createElement("dialog");
  d.className = "keys-dlg";
  d.innerHTML = `<form class="dlg" method="dialog"><h2>Raccourcis clavier</h2>
    <div class="keys">${SHORTCUTS.map(([k, l]) => `<div class="key-row"><kbd>${esc(k)}</kbd><span>${esc(l)}</span></div>`).join("")}</div>
    <div class="muted small">Dans Chrome, ⌘N, ⌘T et ⌘⇧T restent au navigateur ; l'app DRIFTy les laisse passer.</div>
    <div class="foot"><button class="btn primary" value="ok">Fermer</button></div></form>`;
  document.body.append(d);
  d.addEventListener("close", () => d.remove());
  d.showModal();
}

/** Projet de la conversation affichée (discussion en direct ou conversation passée). */
async function currentRepo() {
  const q = new URLSearchParams(location.hash.split("?")[1] || "");
  const c = q.get("c"), s = q.get("s");
  let id = c ? store.chats.find((x) => x.id === c)?.repo_id : null;
  if (!id && s) id = (await api(`/api/sessions/${s}`).catch(() => ({}))).repo_id;
  return store.repos.find((r) => r.id === id) || store.repos.find((r) => !r.hidden);
}

function openTab(kind) {
  const plus = $(".tab-plus");
  if (!plus) return;
  plus.click();
  requestAnimationFrame(() => $(`.tab-menu [data-k="${kind}"]`)?.click());
}

const rows = () => $$("#sb-tree .sb-conv");

export function startKeys() {
  addEventListener("keydown", (e) => {
    if (e.isComposing) return;
    const mod = e.metaKey || e.ctrlKey;
    const k = e.key.toLowerCase();
    let done = true;
    if (mod && !e.shiftKey && k === "k") {
      document.body.classList.remove("sb-hidden");
      const q = $("#sb-q");
      q?.focus();
      q?.select();
    } else if (mod && !e.shiftKey && k === "n") {
      currentRepo().then((repo) => repo && newChatDialog(repo, (chat) => { location.hash = `#/conversations?c=${chat.id}`; }));
    } else if (mod && !e.shiftKey && /^[1-9]$/.test(e.key)) {
      rows()[+e.key - 1]?.click();
    } else if (e.altKey && !mod && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
      const list = rows();
      const at = list.findIndex((r) => r.classList.contains("on"));
      const next = list[at < 0 ? 0 : at + (e.key === "ArrowDown" ? 1 : -1)];
      if (next) { next.click(); next.scrollIntoView({ block: "nearest" }); }
    } else if (mod && !e.shiftKey && k === "l") {
      const ta = $("#detail .composer textarea");
      if ($("#detail.x-on")) $("#detail .tab[data-tab].on, #detail .tab[data-tab]")?.click();
      ta?.focus();
    } else if (mod && !e.shiftKey && k === "b") {
      document.body.classList.toggle("sb-hidden");
    } else if (mod && !e.shiftKey && k === "t") {
      openTab("term");
    } else if (mod && e.shiftKey && k === "t") {
      openTab("web");
    } else if (mod && e.shiftKey && k === "w") {
      $(".xtab.on .xtab-x")?.click();
    } else if (mod && e.shiftKey && k === "g") {
      $("#conv-gh, #chat-gh")?.click();
    } else if (mod && e.shiftKey && k === "d") {
      $("#conv-review, #chat-review")?.click();
    } else if (mod && (e.key === "/" || k === ":")) {
      help();
    } else done = false;
    if (done) { e.preventDefault(); e.stopPropagation(); }
  }, true);
}
