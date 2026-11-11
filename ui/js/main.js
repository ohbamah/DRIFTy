// Disposition : barre latérale persistante + zone principale routée.
import { api, esc, icon, hideTip, toast, loader, lightbox, copyText, $, $$ } from "./lib.js";
import * as usage from "./views/usage.js";
import * as conversations from "./views/conversations.js";
import * as ledger from "./views/ledger.js";
import * as automations from "./views/automations.js";
import * as accounts from "./views/accounts.js";
import { slide } from "./slide.js";
import { mountSidebar } from "./sidebar.js";
import { reload } from "./store.js";
import { startNotifications } from "./notify.js";
import { autoSelects } from "./select.js";
import { startKeys } from "./keys.js";
import { startWatchdog } from "./watchdog.js";

const view = $("#view");
$("#sb-reopen").innerHTML = icon("sidebar");
$("#sb-reopen").addEventListener("click", () => document.body.classList.remove("sb-hidden"));

let current = "";
async function route() {
  hideTip();
  const [path, qs] = location.hash.slice(1).split("?");
  const name = (path || "/").split("/").filter(Boolean)[0] || "usage";
  const params = new URLSearchParams(qs || "");
  if (name !== current) view.innerHTML = loader();
  current = name;
  // Petit écran : la barre se referme quand on ouvre une conversation.
  if (name === "conversations" && innerWidth < 860) document.body.classList.add("sb-hidden");
  try {
    if (name === "conversations") await conversations.show(view, params);
    else if (name === "ledger") await ledger.show(view);
    else if (name === "automations") await automations.show(view);
    else if (name === "accounts") await accounts.show(view);
    else await usage.show(view);
    slide(view);
  } catch (e) {
    view.innerHTML = `<div class="page"><div class="page-in"><div class="callout err">${esc(e.message)}</div></div></div>`;
  }
}
addEventListener("hashchange", route);
// Bouton « Copier » d'un bloc de code (rendu Markdown, voir md.js) : où qu'il soit.
document.addEventListener("click", async (e) => {
  const b = e.target.closest("[data-code-copy]");
  if (!b) return;
  const text = b.closest(".code-block")?.querySelector("pre")?.textContent || "";
  (await copyText(text)) ? toast("Code copié.") : toast("Copie impossible.", true);
});
// Clic sur une vignette d'image jointe : plein écran.
document.addEventListener("click", (e) => {
  const t = e.target.closest(".thumb[data-full]");
  if (t) lightbox(t.dataset.full);
});

// ------------------------------------------------------------ analyse (uniquement sur demande)
const setBusy = (busy) => $$("[data-sync]").forEach((b) => { b.classList.toggle("busy", busy); b.disabled = busy; });
document.addEventListener("click", async (e) => {
  const btn = e.target.closest("[data-sync]");
  if (!btn) return;
  try {
    // « Analyser » un seul dossier (worktree pas encore lu), ou tout.
    const folder = btn.dataset.syncFolder;
    await api(folder ? "/api/sync/folder" : "/api/sync", { method: "POST", ...(folder ? { json: { path: folder } } : {}) });
    setBusy(true);
    for (let i = 0; i < 240; i++) {
      await new Promise((r) => setTimeout(r, 500));
      const s = await api("/api/status");
      if (s.syncing) continue;
      const r = s.last?.ingest;
      toast(r ? `Analysé, ${r.messages.toLocaleString("fr-FR")} nouveaux messages.` : "Analysé.");
      await reload();
      current = "";
      await route();
      $$("[data-sync]").forEach((b) => { b.classList.add("done"); setTimeout(() => b.classList.remove("done"), 800); });
      break;
    }
  } catch (err) { toast(err.message, true); }
  finally { setBusy(false); }
});

mountSidebar($("#sidebar")).then(route);
startNotifications();
autoSelects(document.body); // menus déroulants maison partout
startKeys();
startWatchdog();

// Usage des comptes : relevé par `claude -p /usage` (sans session), toutes les 2 min tant que
// l'app est visible ; le serveur ne relance pas un compte plus d'une fois par minute.
const refreshUsage = () => !document.hidden && api("/api/usage/refresh", { method: "POST" }).catch(() => {});
refreshUsage();
setInterval(refreshUsage, 120_000);

// Guillemets droits : la substitution typographique de macOS (“ ” ‘ ’) est annulée pendant la
// frappe — dans un prompt ou du code, `"` doit rester `"`. Un texte COLLÉ est gardé tel quel.
document.addEventListener("input", (e) => {
  const t = e.target;
  if (!(t instanceof HTMLTextAreaElement || (t instanceof HTMLInputElement && /^(text|search|url)$/.test(t.type)))) return;
  // Seulement ce qui vient d'être tapé (juste avant le curseur) : le reste du champ n'est pas touché.
  if (!e.data || !/[“”‘’]/.test(e.data) || /^insertFrom/.test(e.inputType || "")) return;
  const end = t.selectionStart, start = Math.max(0, end - e.data.length);
  const fixed = t.value.slice(start, end).replace(/[“”]/g, '"').replace(/[‘’]/g, "'"); // même longueur
  t.value = t.value.slice(0, start) + fixed + t.value.slice(end);
  t.setSelectionRange(end, end);
}, true);
