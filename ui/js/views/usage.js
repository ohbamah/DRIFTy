// Accueil : l'usage d'abonnement des deux comptes (5 h, semaine), puis l'activité.
import { api, esc, tok, num, ago, toast, syncButton, $, $$ } from "../lib.js";
import * as analytics from "./analytics.js";
import { countUp } from "../slide.js";

// « dans 2 h 14 · 23:00 » ; au-delà d'un jour : « dans 3 j · lun. 09:00 ».
function resetText(ms) {
  if (!ms) return "";
  const d = new Date(ms);
  const s = Math.max(0, (ms - Date.now()) / 1000);
  const hm = d.toLocaleTimeString("fr-FR", { hour: "2-digit", minute: "2-digit" });
  let left;
  if (s < 3600) left = `${Math.max(1, Math.round(s / 60))} min`;
  else if (s < 86400) left = `${Math.floor(s / 3600)} h ${String(Math.round((s % 3600) / 60)).padStart(2, "0")}`;
  else left = `${Math.floor(s / 86400)} j ${Math.floor((s % 86400) / 3600)} h`;
  const sameDay = d.toDateString() === new Date().toDateString();
  const when = sameDay ? hm : `${d.toLocaleDateString("fr-FR", { weekday: "short" })} ${hm}`;
  return `Reset dans ${left}, ${when}`;
}

// Les deux barres sont TOUJOURS là ; sans relevé elles restent vides.
function meter(label, w) {
  const has = w && w.used_percentage != null;
  const pct = has ? Math.max(0, Math.min(100, w.used_percentage)) : 0;
  const sub = !has ? "Aucun relevé" : w.idle ? "Aucune utilisation" : w.expired ? "Réinitialisée" : resetText(w.resets_at_ms);
  return `<div class="meter${has ? "" : " none"}">
    <div class="meter-top">
      <div><div class="l">${label}</div><div class="r">${sub}</div></div>
      <span class="pct"${has ? ` data-pct="${pct}"` : ""}>${has ? Math.round(pct) + " %" : "—"}</span>
    </div>
    <div class="track" role="meter" aria-label="${esc(label)}" aria-valuemin="0" aria-valuemax="100" aria-valuenow="${pct}">
      ${has ? `<div class="fill${pct >= 80 ? " hot" : ""}" style="width:${pct}%"></div>` : ""}
    </div>
  </div>`;
}

const short = (p) => p.replace(/^\/Users\/[^/]+/, "~");

function card(slot, a) {
  const title = slot === "pro" ? "Pro" : "Perso";
  const badge = slot === "pro" ? "Pro" : "P";
  const configured = a && a.enabled;
  let notice = "";
  if (!configured) {
    notice = `<div class="unotice"><span>Non configuré.</span><a class="btn" href="#/accounts">Configurer</a></div>`;
  } else if (!a.relay_installed && !a.snapshot_at) {
    notice = `<div class="unotice"><span>Active le suivi pour voir l'usage de ce compte.</span><button class="btn primary" data-relay="${slot}">Activer</button></div>`;
  } else if (!a.snapshot_at) {
    notice = `<div class="unotice"><span>${a.seen_at ? "En attente d'une réponse de Claude." : "En attente du premier relevé."}</span></div>`;
  }
  return `<article class="ucard ${slot}">
    <div class="ucard-head">
      <span class="ucard-badge">${badge}</span>
      <div><div class="ucard-name">${esc(configured ? a.label : title)}</div><div class="ucard-sub"${configured ? ` title="${esc(a.config_dir)}"` : ""}>${configured ? esc(short(a.config_dir)) : "—"}</div></div>
      <div class="when">${configured && a.snapshot_at ? `Relevé ${ago(a.snapshot_at)}${a.model ? `<br>${esc(a.model)}` : ""}` : ""}</div>
    </div>
    ${notice}
    ${configured ? `<div class="ucard-stats">
      <span>5 h : <b>${num(a.local_5h.conversations)}</b> conv., <b>${tok(a.local_5h.output)}</b> tokens</span>
      <span>Semaine : <b>${num(a.local_7d.conversations)}</b> conv., <b>${tok(a.local_7d.output)}</b> tokens</span>
    </div>` : ""}
    <div class="meters">
      ${meter("Session (5 h)", configured ? a.five_hour : null)}
      ${meter("Semaine", configured ? a.seven_day : null)}
    </div>
  </article>`;
}

let timer;
async function renderCards(box, first = false) {
  const accs = await api("/api/usage");
  // Rafraîchissement : on ne redessine que si un relevé a changé, ou chaque minute (« il y a … »).
  const key = JSON.stringify(accs.map((a) => [a.slot, a.snapshot_at, a.seen_at, a.five_hour, a.seven_day, a.relay_installed, a.enabled]));
  const minute = Math.floor(Date.now() / 60000);
  if (!first && box.dataset.key === key && box.dataset.minute === String(minute)) return;
  box.dataset.key = key;
  box.dataset.minute = String(minute);
  const by = Object.fromEntries(accs.map((a) => [a.slot, a]));
  box.innerHTML = card("perso", by.perso) + card("pro", by.pro);
  // Remplissage animé à la première apparition seulement, pas aux rafraîchissements.
  box.classList.toggle("anim", first);
  if (first) $$("[data-pct]", box).forEach((el) => countUp(el, +el.dataset.pct));
  $$("[data-relay]", box).forEach((b) => b.addEventListener("click", async () => {
    b.disabled = true;
    try {
      await api(`/api/accounts/${b.dataset.relay}/relay`, { method: "POST" });
      toast("Suivi activé.");
      renderCards(box, true);
    } catch (e) { toast(e.message, true); b.disabled = false; }
  }));
}

export async function show(root) {
  root.innerHTML = `<div class="page"><div class="page-in">
    <div class="page-head"><div><h1>Usage</h1><div class="sub">Limites d'abonnement par compte.</div></div>${syncButton()}</div>
    <section class="usage" id="usage"></section>
    <section id="activity"></section>
  </div></div>`;
  const box = $("#usage", root);
  // Relevé frais des comptes (sans session) à l'ouverture de la page, puis redessin.
  api("/api/usage/refresh", { method: "POST" }).then((r) => { if (r.refreshed?.length && document.body.contains(box)) show(root); }).catch(() => {});
  await Promise.all([renderCards(box, true), analytics.render($("#activity", root))]);
  // Les jauges se relisent toutes les 5 s : simple lecture du relevé local, aucune synchronisation.
  clearInterval(timer);
  timer = setInterval(() => (document.body.contains(box) ? renderCards(box).catch(() => {}) : clearInterval(timer)), 5000);
}
