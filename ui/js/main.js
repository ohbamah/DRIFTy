// Disposition : barre latérale persistante + zone principale routée.
import { api, esc, icon, hideTip, toast, loader, lightbox, $, $$ } from "./lib.js";
import * as usage from "./views/usage.js";
import * as conversations from "./views/conversations.js";
import * as ledger from "./views/ledger.js";
import * as accounts from "./views/accounts.js";
import { slide } from "./slide.js";
import { mountSidebar } from "./sidebar.js";
import { reload } from "./store.js";
import { startNotifications } from "./notify.js";

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
    else if (name === "accounts") await accounts.show(view);
    else await usage.show(view);
    slide(view);
  } catch (e) {
    view.innerHTML = `<div class="page"><div class="page-in"><div class="callout err">${esc(e.message)}</div></div></div>`;
  }
}
addEventListener("hashchange", route);
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
    await api("/api/sync", { method: "POST" });
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
