// Outils partagés : API, formats, échappement, infobulle, toast, icônes.

export const $ = (s, el = document) => el.querySelector(s);
export const $$ = (s, el = document) => [...el.querySelectorAll(s)];

export { esc } from "./text.js";
import { esc } from "./text.js";
import { icon } from "./icons.js";
export { icon };

export async function api(path, opts = {}) {
  const init = { ...opts };
  if (init.json !== undefined) {
    init.body = JSON.stringify(init.json);
    init.headers = { "content-type": "application/json", ...(init.headers || {}) };
    delete init.json;
  }
  const r = await fetch(path, init);
  const body = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(body.error || `HTTP ${r.status}`);
  return body;
}

const nf = new Intl.NumberFormat("fr-FR");
export const num = (n) => nf.format(Number(n) || 0);
export function tok(n) {
  n = Number(n) || 0;
  const a = Math.abs(n);
  const f = (x, d) => x.toFixed(d).replace(".", ",");
  if (a >= 1e9) return f(n / 1e9, 2) + " G";
  if (a >= 1e6) return f(n / 1e6, 1) + " M";
  if (a >= 1e4) return Math.round(n / 1e3) + " k";
  return nf.format(n);
}
export function dur(ms) {
  ms = Math.max(0, Number(ms) || 0);
  const s = Math.round(ms / 1000);
  if (s < 60) return s + " s";
  const m = Math.floor(s / 60);
  if (m < 60) return m + " min";
  const h = Math.floor(m / 60);
  return m % 60 ? `${h} h ${String(m % 60).padStart(2, "0")}` : `${h} h`;
}
/** Fin d'un tour : « à 15:51 · 12 min » (avec le jour s'il n'est pas aujourd'hui). Un seul point
 * pour la discussion en direct et le transcript d'une conversation passée. */
export function turnEnd(ts, durationMs) {
  const end = new Date(ts || Date.now());
  const today = end.toDateString() === new Date().toDateString();
  const when = end.toLocaleString("fr-FR", today ? { hour: "2-digit", minute: "2-digit" } : { weekday: "short", day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
  return `à ${when}${durationMs ? ` · ${dur(durationMs)}` : ""}`;
}

/** Copie un texte ; `true` si c'est fait. Repli sur une sélection quand le presse-papiers
 * moderne manque ou refuse (certaines vues web de l'app). */
export async function copyText(text) {
  try { await navigator.clipboard.writeText(text); return true; } catch {}
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.setAttribute("readonly", "");
  ta.style.cssText = "position:fixed;top:-1000px;opacity:0";
  (document.querySelector("dialog[open]") || document.body).append(ta);
  ta.select();
  let ok = false;
  try { ok = document.execCommand("copy"); } catch {}
  ta.remove();
  return ok;
}

export const usd = (x) => (x == null ? "—" : x > 0 && x < 0.005 ? "< $0,01" : "$" + nf.format(Math.round(x * 100) / 100));
export const pct = (x, d = 1) => (x == null ? "—" : (x * 100).toFixed(d).replace(".", ",") + " %");
export const date = (ts) =>
  ts ? new Date(ts).toLocaleString("fr-FR", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" }) : "—";
export function ago(ts) {
  if (!ts) return "";
  const s = (Date.now() - ts) / 1000;
  if (s < 60) return "à l'instant";
  if (s < 3600) return `il y a ${Math.floor(s / 60)} min`;
  if (s < 86400) return `il y a ${Math.floor(s / 3600)} h`;
  if (s < 86400 * 7) return `il y a ${Math.floor(s / 86400)} j`;
  return new Date(ts).toLocaleDateString("fr-FR", { day: "numeric", month: "short" });
}

/** Bouton « Analyser », placé en haut des pages dont il met à jour les données. */
export const syncButton = () => `<button class="btn sync-btn" type="button" data-sync>${icon("sync")}Analyser</button>`;

/** Nom lisible d'un modèle, d'après la liste de Claude Code (`list`) ; à défaut, `claude-opus-5-5` → « Opus 5.5 ». */
export function modelLabel(id, list = []) {
  if (!id) return "Par défaut";
  const hit = list.find((m) => m.value === id) || list.find((m) => m.resolved === id && m.value !== "default");
  if (hit) return hit.name;
  const m = /^claude-([a-z]+)-(\d+)(?:-(\d{1,2}))?(?:-\d{8})?$/.exec(id); // la date finale n'est pas une version
  if (!m) return id;
  return `${m[1][0].toUpperCase()}${m[1].slice(1)} ${m[2]}${m[3] ? "." + m[3] : ""}`;
}

/** `+53 −123` d'une modification de fichier (vert / rouge). */
export const diffStat = (d) =>
  d ? `<span class="diffstat">${d[0] ? `<span class="add">+${d[0]}</span>` : ""}${d[1] ? `<span class="del">−${d[1]}</span>` : ""}</span>` : "";

/** Vignettes d'images jointes, au-dessus d'un message. */
export const imageStrip = (urls) =>
  urls.length ? `<div class="msg-imgs">${urls.map((u) => `<button type="button" class="thumb" data-full="${esc(u)}"><img src="${esc(u)}" alt="Image jointe" loading="lazy"></button>`).join("")}</div>` : "";

/** Visualiseur plein écran (clic ou Échap pour fermer). */
export function lightbox(src) {
  const el = document.createElement("div");
  el.className = "lightbox";
  el.innerHTML = `<img src="${esc(src)}" alt="Image jointe"><button type="button" class="lb-close" aria-label="Fermer">${icon("close")}</button>`;
  const close = () => { el.remove(); removeEventListener("keydown", onKey); };
  const onKey = (e) => e.key === "Escape" && close();
  el.addEventListener("click", close);
  addEventListener("keydown", onKey);
  document.body.append(el);
}

/** Thème : `system` (réglage du Mac), `light` ou `dark`. Gardé dans ce navigateur. */
export function getTheme() {
  try { return localStorage.getItem("drifty.theme") || "system"; } catch { return "system"; }
}
export function setTheme(t) {
  document.documentElement.classList.add("theme-fade");
  setTimeout(() => document.documentElement.classList.remove("theme-fade"), 300);
  if (t === "light" || t === "dark") document.documentElement.dataset.theme = t;
  else delete document.documentElement.dataset.theme;
  try { t === "system" ? localStorage.removeItem("drifty.theme") : localStorage.setItem("drifty.theme", t); } catch {}
}

/** Loader circulaire qui remplace un contenu en cours de chargement. */
export const loader = (small = false) =>
  `<div class="loader${small ? " sm" : ""}" role="status" aria-label="Chargement"><span class="spin"></span></div>`;

// ------------------------------------------------------------ infobulle, toast
const tip = $("#tip");
export function showTip(e, html) {
  tip.innerHTML = html;
  tip.hidden = false;
  const r = tip.getBoundingClientRect();
  let x = e.clientX + 14, y = e.clientY + 14;
  if (x + r.width > innerWidth - 8) x = e.clientX - r.width - 14;
  if (y + r.height > innerHeight - 8) y = e.clientY - r.height - 14;
  tip.style.left = x + "px";
  tip.style.top = y + "px";
}
export const hideTip = () => (tip.hidden = true);

let toastTimer;
export function toast(text, isError = false) {
  const t = $("#toast");
  t.textContent = text;
  t.className = "toast" + (isError ? " err" : "");
  t.hidden = true;
  void t.offsetWidth; // relance l'animation d'entrée
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.hidden = true), isError ? 6000 : 3500);
}

// ------------------------------------------------------------ icônes (traits 1,75)

// ------------------------------------------------------------ logo de projet
export const PROJECT_COLORS = ["#9d7bf5", "#3987e5", "#1baf7a", "#e9a23b", "#e8628c", "#ef6b6b", "#2bb5c4", "#7a8394"];
function hash(s) {
  let h = 0;
  for (const c of s) h = (h * 31 + c.charCodeAt(0)) | 0;
  return Math.abs(h);
}
export function logo(p, size = "") {
  const cls = "logo" + (size ? " " + size : "");
  if (p.icon && p.icon.startsWith("data:image/")) return `<span class="${cls}"><img src="${esc(p.icon)}" alt=""></span>`;
  if (p.icon) return `<span class="${cls} emoji">${esc(p.icon)}</span>`;
  const color = p.color || PROJECT_COLORS[hash(p.key || p.name) % PROJECT_COLORS.length];
  const letters = (p.name || "?").replace(/[^\p{L}\p{N}]/gu, "").slice(0, 2);
  return `<span class="${cls}" style="background:${esc(color)}">${esc(letters.charAt(0).toUpperCase() + letters.slice(1, 2).toLowerCase())}</span>`;
}
/** Pastilles d'état d'un projet ou d'un worktree : verte = discussions actives, rouge = terminées non lues. */
const badgeNum = (n) => (n > 9 ? "9+" : String(n));
export function badges(active, unread, inline = false) {
  const cls = inline ? " inline" : "";
  return (
    (active ? `<span class="nbadge active${cls}" title="${active} discussion${active > 1 ? "s" : ""} en cours">${badgeNum(active)}</span>` : "") +
    (unread ? `<span class="nbadge unread${cls}" title="${unread} discussion${unread > 1 ? "s" : ""} terminée${unread > 1 ? "s" : ""}, non lue${unread > 1 ? "s" : ""}">${badgeNum(unread)}</span>` : "")
  );
}

export const slotChip = (slot) =>
  slot ? `<span class="chip ${esc(slot)}">${slot === "pro" ? "Pro" : "Perso"}</span>` : "";
