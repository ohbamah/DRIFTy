// Réglages d'un projet : nom, logo (emoji ou image), couleur, compte, masquage.
import { api, esc, icon, logo, toast, PROJECT_COLORS, $, $$ } from "../lib.js";
import { store, setRepos } from "../store.js";
import { soundList, soundLabel, loadNotifyCfg, play } from "../notify.js";

// ------------------------------------------------------------ personnalisation d'un projet
export function editProject(repo, hint = "") {
  const enabled = store.accounts.filter((a) => a.enabled);
  const dlg = document.createElement("dialog");
  let iconValue = repo.icon || "";
  let color = repo.color || "";
  dlg.innerHTML = `<form class="dlg" method="dialog">
    <h2>Projet</h2>
    ${hint ? `<div class="callout" style="margin:0">${esc(hint)}</div>` : ""}
    <div class="logo-row">
      <span id="preview"></span>
      <div style="display:flex;flex-direction:column;gap:8px;flex:1">
        <div style="display:flex;gap:8px">
          <input class="field" id="emoji" placeholder="Emoji" maxlength="4" style="width:84px" value="${iconValue && !iconValue.startsWith("data:") ? esc(iconValue) : ""}">
          <label class="btn" style="flex-direction:row;color:var(--ink)">${icon("upload")}Image<input type="file" accept="image/*" id="file" hidden></label>
          <button class="btn" type="button" id="clear-icon">Retirer</button>
        </div>
        <div class="swatches">${PROJECT_COLORS.map((c) => `<button type="button" class="swatch${c === color ? " on" : ""}" data-c="${c}" style="background:${c}" aria-label="Couleur ${c}"></button>`).join("")}</div>
      </div>
    </div>
    <label>Nom<input class="field" id="name" value="${esc(repo.name)}" maxlength="60" required></label>
    <label>Compte
      <select class="field" id="slot">
        ${["perso", "pro"].map((s) => {
          const a = enabled.find((x) => x.slot === s);
          return `<option value="${s}"${repo.slot === s ? " selected" : ""}${a ? "" : " disabled"}>${s === "pro" ? "Pro" : "Perso"}${a ? "" : " (non configuré)"}</option>`;
        }).join("")}
      </select>
      
    </label>
    <label>Son « agent qui a fini »<span class="snd-row"><select class="field" id="snd-done"></select><button class="icon-btn" type="button" data-play="snd-done">▶</button></span></label>
    <label>Son « autorisation demandée »<span class="snd-row"><select class="field" id="snd-permission"></select><button class="icon-btn" type="button" data-play="snd-permission">▶</button></span></label>
    <div class="setup-field">
      <div class="setup-head"><b>Mise en place d'un nouveau worktree</b><span style="flex:1"></span><button class="btn small" type="button" id="setup-ai">${icon("sparkle")}Proposer par l'IA</button></div>
      <textarea class="field area mono" id="setup" rows="6" wrap="off" spellcheck="false" placeholder="# Lancé par bash -e dans chaque nouveau worktree, avant le premier message.\n# cp \"$CLAUDIUS_ROOT_PATH/.env\" .env\n# bundle install"></textarea>
      <div class="muted small" id="setup-why">Variables : <code>$CLAUDIUS_ROOT_PATH</code> (dossier principal), <code>$CLAUDIUS_WORKSPACE_NAME</code>, <code>$PORT</code>. Les scripts Conductor marchent tels quels.</div>
    </div>
    <label class="check"><input type="checkbox" id="hidden"${repo.hidden ? " checked" : ""}> Masquer</label>
    <div class="muted small" title="${esc(repo.root)}" style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${esc(repo.root)}</div>
    <div class="foot"><button class="btn" value="cancel" formnovalidate>Annuler</button><button class="btn primary" value="save" id="save">Enregistrer</button></div>
  </form>`;
  document.body.append(dlg);
  // Sons du projet : « comme les réglages globaux » par défaut.
  loadNotifyCfg(true).then((cfg) => {
    const own = cfg.repos[repo.id]?.sounds || {};
    for (const ev of ["done", "permission"]) {
      const sel = $(`#snd-${ev}`, dlg);
      sel.innerHTML = `<option value="">Comme les réglages globaux (${esc(soundLabel(cfg.global.sounds[ev]))})</option>` +
        soundList().map(([k, l]) => `<option value="${esc(k)}"${own[ev] === k ? " selected" : ""}>${esc(l)}</option>`).join("");
    }
    $$("[data-play]", dlg).forEach((b) => b.addEventListener("click", () => {
      const v = $(`#${b.dataset.play}`, dlg).value || cfg.global.sounds[b.dataset.play.slice(4)];
      play(v, cfg.global.volume);
    }));
  });
  // Script de mise en place : proposé par l'IA à la première ouverture (puis gardé comme proposition).
  let setupLoaded = false;
  const setupTa = $("#setup", dlg), setupBtn = $("#setup-ai", dlg);
  const propose = async () => {
    setupBtn.disabled = true;
    setupBtn.innerHTML = `<span class="spin" style="width:12px;height:12px;border-width:2px"></span> Analyse du dépôt…`;
    try {
      const r = await api(`/api/repos/${repo.id}/setup/analyze`, { method: "POST" });
      setupTa.value = r.script;
      if (r.why) $("#setup-why", dlg).textContent = r.why;
    } catch (err) { toast(err.message, true); }
    setupBtn.disabled = false;
    setupBtn.innerHTML = `${icon("sparkle")}Proposer par l'IA`;
  };
  setupBtn.addEventListener("click", propose);
  api(`/api/repos/${repo.id}/setup`).then((cfg) => {
    setupLoaded = true;
    if (cfg.configured) setupTa.value = cfg.script || "";
    else if (cfg.proposal) setupTa.value = cfg.proposal.script || "";
    else propose();
  }).catch(() => {});
  const preview = () => ($("#preview", dlg).innerHTML = logo({ ...repo, icon: iconValue, color, name: $("#name", dlg).value || repo.name }, "lg"));
  preview();
  $("#name", dlg).addEventListener("input", preview);
  $("#emoji", dlg).addEventListener("input", (e) => { iconValue = e.target.value.trim(); preview(); });
  $("#clear-icon", dlg).addEventListener("click", () => { iconValue = ""; $("#emoji", dlg).value = ""; preview(); });
  $$(".swatch", dlg).forEach((b) => b.addEventListener("click", () => {
    color = b.dataset.c;
    $$(".swatch", dlg).forEach((x) => x.classList.toggle("on", x === b));
    preview();
  }));
  $("#file", dlg).addEventListener("change", async (e) => {
    const f = e.target.files[0];
    if (!f) return;
    try { iconValue = await shrink(f); preview(); } catch (err) { toast(err.message, true); }
  });
  dlg.addEventListener("close", async () => {
    if (dlg.returnValue === "save") {
      try {
        setRepos(await api(`/api/repos/${repo.id}`, {
          method: "PUT",
          json: { name: $("#name", dlg).value, icon: iconValue, color, slot: $("#slot", dlg).value, hidden: $("#hidden", dlg).checked },
        }));
        if (setupLoaded) await api(`/api/repos/${repo.id}/setup`, { method: "PUT", json: { script: setupTa.value } });
        const sounds = { done: $("#snd-done", dlg).value || null, permission: $("#snd-permission", dlg).value || null };
        await api(`/api/settings/notify/repo/${repo.id}`, { method: "PUT", json: { sounds } });
        await loadNotifyCfg(true);
        toast("Enregistré.");
      } catch (err) { toast(err.message, true); }
    }
    dlg.remove();
  });
  dlg.showModal();
}

// Une image de logo est réduite à 128 px avant d'être enregistrée.
function shrink(file) {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => {
      const c = document.createElement("canvas");
      c.width = c.height = 128;
      const s = Math.min(img.width, img.height);
      c.getContext("2d").drawImage(img, (img.width - s) / 2, (img.height - s) / 2, s, s, 0, 0, 128, 128);
      URL.revokeObjectURL(img.src);
      resolve(c.toDataURL("image/png"));
    };
    img.onerror = () => reject(new Error("Image illisible."));
    img.src = URL.createObjectURL(file);
  });
}
