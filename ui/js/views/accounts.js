// Réglages : apparence, puis comptes (un perso et un pro, pas plus).
import { api, esc, icon, toast, getTheme, setTheme, $$ } from "../lib.js";
import { moveTo } from "../slide.js";
import { EVENTS, soundList, loadNotifyCfg, play, askPermission } from "../notify.js";

export async function show(root) {
  const accs = await api("/api/accounts");
  const by = Object.fromEntries(accs.map((a) => [a.slot, a]));
  const card = (slot, title, hint) => {
    const a = by[slot] || { label: title, config_dir: "", enabled: false };
    return `<form class="panel acc" data-slot="${slot}">
      <h2 style="margin:0">${title}</h2>
      <label>Nom affiché<input class="field" name="label" value="${esc(a.label)}"></label>
      <label>Dossier Claude<input class="field" name="config_dir" value="${esc(a.config_dir)}" placeholder="${hint}"></label>
      <div style="display:flex;gap:8px">
        <button class="btn primary">Enregistrer</button>
        ${by[slot] ? `<button class="btn" type="button" data-remove="${slot}">Retirer</button>` : ""}
      </div>
    </form>`;
  };
  root.innerHTML = `<div class="page"><div class="page-in" style="max-width:860px">
    <div class="page-head"><div><h1>Réglages</h1></div></div>
    <section class="panel settings-row">
      <div><h2 style="margin:0 0 2px">Apparence</h2><div class="muted small">Système suit le réglage du Mac.</div></div>
      <div class="seg" id="theme">${[["system", "Système"], ["light", "Clair"], ["dark", "Sombre"]].map(([v, l]) => `<button type="button" data-v="${v}" class="${getTheme() === v ? "on" : ""}">${l}</button>`).join("")}</div>
    </section>
    <section class="panel" id="notify-panel" style="margin-top:16px"></section>
    <h2 class="section-title" style="margin:32px 0 4px">Comptes</h2>
    <div class="sub" style="margin-bottom:14px">Un perso, un pro. Chaque projet choisit le sien.</div>
    <div class="callout">Le compte pro est celui de ton employeur. Connexion : <code>CLAUDE_CONFIG_DIR=~/.claude-pro claude</code></div>
    <div class="grid">${card("perso", "Perso", "~/.claude")}${card("pro", "Pro", "~/.claude-pro")}</div>
  </div></div>`;
  drawNotify(root.querySelector("#notify-panel"));
  $$("#theme button", root).forEach((b) => b.addEventListener("click", () => {
    moveTo(b);
    setTheme(b.dataset.v);
  }));
  $$("form[data-slot]", root).forEach((f) => f.addEventListener("submit", async (e) => {
    e.preventDefault();
    const fd = new FormData(f);
    const dir = String(fd.get("config_dir") || "").trim();
    try {
      if (!dir) throw new Error("Dossier requis.");
      await api("/api/accounts", {
        method: "PUT",
        json: { slot: f.dataset.slot, label: String(fd.get("label") || f.dataset.slot), config_dir: dir, enabled: true },
      });
      toast("Compte enregistré.");
      show(root);
    } catch (err) { toast(err.message, true); }
  }));
  $$("[data-remove]", root).forEach((b) => b.addEventListener("click", async () => {
    if (!confirm(`Retirer le compte ${b.dataset.remove} ?`)) return;
    try {
      await api(`/api/accounts/${b.dataset.remove}`, { method: "DELETE" });
      toast("Compte retiré.");
      show(root);
    } catch (err) { toast(err.message, true); }
  }));
}

// ------------------------------------------------------------ notifications
const soundSelect = (event, value, withInherit = false) =>
  `<select class="field snd" data-ev="${event}">${withInherit ? `<option value="">Comme les réglages globaux</option>` : ""}${soundList().map(([k, l]) => `<option value="${esc(k)}"${value === k ? " selected" : ""}>${esc(l)}</option>`).join("")}</select>`;

async function drawNotify(box) {
  const cfg = await loadNotifyCfg(true);
  const g = cfg.global;
  const perm = "Notification" in window ? Notification.permission : "unsupported";
  box.innerHTML = `
    <div class="settings-row" style="margin-bottom:14px">
      <div><h2 style="margin:0 0 2px">Notifications</h2><div class="muted small">Sons et alertes système. Les sons « agent » se règlent aussi par projet.</div></div>
      <label class="check"><input type="checkbox" id="nt-on"${g.enabled ? " checked" : ""}> Activées</label>
    </div>
    <div class="nt-grid">
      ${Object.entries(EVENTS).map(([k, l]) => `<div class="nt-row"><span>${l}</span>${soundSelect(k, g.sounds[k])}<button class="icon-btn" type="button" data-play="${k}" title="Écouter">▶</button></div>`).join("")}
    </div>
    <div class="nt-foot">
      <label>Seuil des limites <select class="field" id="nt-th">${[80, 90, 100].map((v) => `<option value="${v}"${g.threshold === v ? " selected" : ""}>${v} %</option>`).join("")}</select></label>
      <label>Volume <input type="range" id="nt-vol" min="0" max="1" step="0.05" value="${g.volume}"></label>
      ${perm === "granted" ? `<span class="muted small">Notifications système autorisées.</span>` : perm === "unsupported" ? "" : `<button class="btn" type="button" id="nt-perm">Autoriser les notifications système</button>`}
    </div>
    <div class="nt-custom">
      <div class="settings-row"><b>Sons personnalisés</b><label class="btn">${icon("upload")}Ajouter un son<input type="file" id="nt-add" accept="audio/mpeg,audio/wav,audio/ogg,audio/mp4,audio/x-m4a,audio/aac,audio/webm,.mp3,.wav,.ogg,.m4a,.aac" hidden></label></div>
      ${cfg.custom.length ? cfg.custom.map((s) => `<div class="nt-row"><span>${esc(s.name)}</span><span class="muted small">${esc(s.ext)}</span><button class="icon-btn" type="button" data-play-custom="${esc(s.id)}" title="Écouter">▶</button><button class="icon-btn" type="button" data-del-sound="${esc(s.id)}" title="Supprimer">${icon("close")}</button></div>`).join("")
        : `<div class="muted small">mp3, wav, ogg ou m4a, 5 Mo au plus. Ils s'ajoutent aux choix de chaque événement, ici et dans les projets.</div>`}
    </div>`;
  const save = async () => {
    const sounds = {};
    box.querySelectorAll(".snd").forEach((s) => (sounds[s.dataset.ev] = s.value));
    const next = { enabled: box.querySelector("#nt-on").checked, threshold: +box.querySelector("#nt-th").value, volume: +box.querySelector("#nt-vol").value, sounds };
    try { await api("/api/settings/notify", { method: "PUT", json: next }); await loadNotifyCfg(true); } catch (e) { toast(e.message, true); }
  };
  box.querySelectorAll("select, input").forEach((el) => el.addEventListener("change", save));
  box.querySelectorAll("[data-play]").forEach((b) => b.addEventListener("click", () =>
    play(box.querySelector(`.snd[data-ev="${b.dataset.play}"]`).value, +box.querySelector("#nt-vol").value)));
  box.querySelector("#nt-perm")?.addEventListener("click", async () => { await askPermission(); drawNotify(box); });
  box.querySelectorAll("[data-play-custom]").forEach((b) => b.addEventListener("click", () => play("u:" + b.dataset.playCustom, +box.querySelector("#nt-vol").value)));
  box.querySelectorAll("[data-del-sound]").forEach((b) => b.addEventListener("click", async () => {
    if (!confirm("Supprimer ce son ? Les événements qui l'utilisent reprendront le son par défaut.")) return;
    try { await api(`/api/sounds/${b.dataset.delSound}`, { method: "DELETE" }); drawNotify(box); } catch (e) { toast(e.message, true); }
  }));
  box.querySelector("#nt-add").addEventListener("change", async (e) => {
    const f = e.target.files[0];
    if (!f) return;
    const name = prompt("Nom du son", f.name.replace(/\.[^.]+$/, "").slice(0, 40));
    if (!name?.trim()) return;
    try {
      const r = await fetch(`/api/sounds?name=${encodeURIComponent(name.trim())}`, { method: "POST", headers: { "Content-Type": f.type || "audio/mpeg" }, body: f });
      const j = await r.json().catch(() => ({}));
      if (!r.ok || j.error) throw new Error(j.error || `HTTP ${r.status}`);
      toast(`Son « ${j.name} » ajouté.`);
      drawNotify(box);
    } catch (err) { toast(err.message, true); }
  });
}

