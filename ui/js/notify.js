// Notifications et sons : limite 5 h, limite hebdo, agent qui a fini, autorisation
// demandée. Sons synthétisés ou ajoutés par l'utilisateur (`u:<id>`), réglables
// globalement puis par projet.
import { api, toast } from "./lib.js";
import { store, subscribe } from "./store.js";

export const EVENTS = {
  quota5h: "Limite 5 h atteinte",
  quotaWeek: "Limite de la semaine atteinte",
  done: "Agent qui a fini",
  permission: "Autorisation demandée",
};
export const SOUNDS = { cloche: "Cloche", goutte: "Goutte", carillon: "Carillon", bip: "Bip", doux: "Doux", faahhh: "Faahhh", carhonk: "CarHonk", aucun: "Aucun" };

/** Tous les sons proposés : préinstallés, puis ceux de l'utilisateur. */
export function soundList() {
  const own = (cfg?.custom || []).map((s) => [`u:${s.id}`, s.name]);
  const base = Object.entries(SOUNDS);
  return [...base.slice(0, -1), ...own, base[base.length - 1]];
}
export const soundLabel = (k) => soundList().find(([key]) => key === k)?.[1] || SOUNDS.goutte;
const DEFAULTS = {
  enabled: true, threshold: 100, volume: 0.6,
  sounds: { quota5h: "carillon", quotaWeek: "cloche", done: "goutte", permission: "bip" },
};

let cfg = null;
export async function loadNotifyCfg(force = false) {
  if (!cfg || force) {
    const r = await api("/api/settings/notify").catch(() => ({}));
    const custom = await api("/api/sounds").catch(() => []);
    cfg = { global: { ...DEFAULTS, ...(r.global || {}), sounds: { ...DEFAULTS.sounds, ...(r.global?.sounds || {}) } }, repos: r.repos || {}, custom };
  }
  return cfg;
}

// ------------------------------------------------------------ sons (Web Audio)
let ctx = null;
const buffers = new Map(); // sons en fichier (préinstallés ou de l'utilisateur), décodés une fois
const BUNDLED = { faahhh: "/ui/sounds/faahhh.mp3", carhonk: "/ui/sounds/carhonk.mp3" }; // préinstallés en fichier
async function playFile(url, volume) {
  let buf = buffers.get(url);
  if (!buf) {
    const r = await fetch(url);
    if (!r.ok) return play("goutte", volume); // son supprimé : on retombe sur le son par défaut
    buf = await ctx.decodeAudioData(await r.arrayBuffer());
    buffers.set(url, buf);
  }
  const src = ctx.createBufferSource(), g = ctx.createGain();
  src.buffer = buf;
  g.gain.value = volume;
  src.connect(g).connect(ctx.destination);
  src.start();
}

export function play(name, volume = cfg?.global.volume ?? 0.6) {
  if (!name || name === "aucun") return;
  ctx = ctx || new (window.AudioContext || window.webkitAudioContext)();
  if (name.startsWith("u:") || BUNDLED[name]) {
    playFile(BUNDLED[name] || `/api/sounds/${encodeURIComponent(name.slice(2))}`, volume).catch(() => {});
    return;
  }
  const t0 = ctx.currentTime;
  const note = (freq, start, dur, type = "sine", to = null, gain = 1) => {
    const o = ctx.createOscillator(), g = ctx.createGain();
    o.type = type;
    o.frequency.setValueAtTime(freq, t0 + start);
    if (to) o.frequency.exponentialRampToValueAtTime(to, t0 + start + dur);
    g.gain.setValueAtTime(0.0001, t0 + start);
    g.gain.exponentialRampToValueAtTime(0.25 * volume * gain, t0 + start + 0.01);
    g.gain.exponentialRampToValueAtTime(0.0001, t0 + start + dur);
    o.connect(g).connect(ctx.destination);
    o.start(t0 + start);
    o.stop(t0 + start + dur + 0.05);
  };
  ({
    cloche: () => { note(880, 0, 1.2); note(1760, 0, 0.6, "sine", null, 0.35); },
    goutte: () => note(1300, 0, 0.25, "sine", 420),
    carillon: () => { note(784, 0, 0.5); note(1047, 0.18, 0.7); },
    bip: () => { note(1000, 0, 0.09, "square", null, 0.5); note(1000, 0.14, 0.09, "square", null, 0.5); },
    doux: () => note(440, 0, 0.6, "triangle"),
  })[name]?.();
}


/** Son effectif d'un événement pour un projet (réglage du projet, sinon global). */
function soundFor(event, repoId) {
  const o = repoId != null ? cfg.repos[repoId]?.sounds?.[event] : null;
  return o || cfg.global.sounds[event];
}

function fire(event, title, body, repoId, href) {
  if (!cfg?.global.enabled) return;
  play(soundFor(event, repoId));
  if ("Notification" in window && Notification.permission === "granted") {
    const n = new Notification(title, { body, tag: event + ":" + (href || ""), silent: true });
    n.onclick = () => { window.focus(); if (href) location.hash = href; n.close(); };
  } else {
    toast(`${title}${body ? " — " + body : ""}`);
  }
}

export function askPermission() {
  if (!("Notification" in window)) return Promise.resolve("unsupported");
  return Notification.requestPermission();
}

// ------------------------------------------------------------ surveillance
let prev = null;
function watchChats() {
  const now = new Map(store.chats.map((c) => [c.id, c]));
  if (prev) {
    const viewing = new URLSearchParams(location.hash.split("?")[1] || "").get("c");
    for (const c of now.values()) {
      const p = prev.get(c.id);
      if (!p) continue;
      // Agent qui a fini son tour (sauf si on regarde déjà cette discussion).
      if (p.status === "working" && c.status !== "working" && !(c.id === viewing && !document.hidden)) {
        fire("done", "Agent terminé", c.name, c.repo_id, `#/conversations?c=${c.id}`);
      }
      if ((c.pending || 0) > (p.pending || 0)) {
        fire("permission", "Autorisation demandée", c.name, c.repo_id, `#/conversations?c=${c.id}`);
      }
    }
  }
  prev = now;
}

// Limites : une seule notification par fenêtre (clé = compte + moment du reset), gardée entre deux ouvertures.
const seenKey = "drifty.notified";
const seen = () => { try { return new Set(JSON.parse(localStorage.getItem(seenKey) || "[]")); } catch { return new Set(); } };
function markSeen(k) {
  const s = seen();
  s.add(k);
  try { localStorage.setItem(seenKey, JSON.stringify([...s].slice(-50))); } catch {}
}
async function watchUsage() {
  if (!cfg?.global.enabled) return;
  const accs = await api("/api/usage").catch(() => []);
  const already = seen();
  for (const a of accs.filter((x) => x.enabled)) {
    for (const [field, event, label] of [["five_hour", "quota5h", "session 5 h"], ["seven_day", "quotaWeek", "semaine"]]) {
      const w = a[field];
      if (!w || w.used_percentage == null || w.used_percentage < cfg.global.threshold) continue;
      const k = `${a.slot}:${field}:${w.resets_at_ms}`;
      if (already.has(k)) continue;
      markSeen(k);
      fire(event, `${a.label} : ${label} à ${Math.round(w.used_percentage)} %`, w.resets_at_ms ? `Reset ${new Date(w.resets_at_ms).toLocaleString("fr-FR", { weekday: "short", hour: "2-digit", minute: "2-digit" })}` : "", null, "#/");
    }
  }
}

export async function startNotifications() {
  await loadNotifyCfg();
  subscribe(watchChats);
  watchChats();
  watchUsage();
  setInterval(watchUsage, 15000);
}
