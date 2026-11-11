// Chat dans l'app : un processus `claude` piloté par le démon, suivi en direct.
import { api, esc, usd, tok, turnEnd, icon, slotChip, toast, loader, imageStrip, diffStat, modelLabel, $, $$ } from "../lib.js";
import { md, stableLength } from "../md.js";
import { attachImprove } from "../improve.js";
import { attachSlash } from "../slash.js";
import { openBtw, forkMessage } from "../btw.js";
import { token, withElements, userText } from "../elements.js";
import { toBottom, jumpButton } from "../jump.js";
import { replyActions, bindReplies } from "../reply.js";
import { confirmBox } from "../confirm.js";
import { mountAsk, askResolved } from "../ask.js";
import { mountPlan, planChoices, planOutcome } from "../plan.js";
import { bashRow, setupRow, appendTail, asksPassword } from "../bashrow.js";

let source = null; // flux SSE du chat affiché (un seul à la fois)

export function closeStream() {
  if (source) source.close();
  source = null;
}

// ------------------------------------------------------------ modèles
// La liste vient de Claude Code (par compte) : `{value, resolved, name, description}`.
const modelLists = new Map();
export async function loadModels(slot = "") {
  if (!modelLists.has(slot)) modelLists.set(slot, api(`/api/models?slot=${encodeURIComponent(slot)}`).catch(() => []));
  return modelLists.get(slot);
}

/** Sélecteur de modèle (dans la barre de saisie). `current` : modèle en service (id résolu ou alias).
 *  `onPick(value)` est appelé au choix ; rend une fonction pour refléter un modèle annoncé par Claude Code. */
export async function modelPicker(el, slot, current, onPick) {
  const list = await loadModels(slot || "");
  const opts = list.filter((m) => m.value !== "default");
  const selected = (id) => opts.find((m) => m.value === id) || opts.find((m) => m.resolved === id);
  const cur = selected(current);
  el.innerHTML = `<select class="model-sel" title="Modèle">
    <option value=""${cur ? "" : " selected"}>${esc(list.find((m) => m.value === "default")?.name || "Par défaut")}</option>
    ${opts.map((m) => `<option value="${esc(m.value)}" title="${esc(m.description || "")}"${cur === m ? " selected" : ""}>${esc(m.name)}</option>`).join("")}
  </select>`;
  const sel = el.querySelector("select");
  let prev = sel.value;
  sel.addEventListener("change", async () => {
    try { await onPick(sel.value); prev = sel.value; } catch (e) { toast(e.message, true); sel.value = prev; }
  });
  return (model) => {
    const m = selected(model);
    if (m) sel.value = prev = m.value;
  };
}

// ------------------------------------------------------------ nouvelle discussion
export function newChatDialog(repo, onStarted) {
  const dlg = document.createElement("dialog");
  let mode = repo.chat_mode || "worktree";
  dlg.innerHTML = `<form class="dlg" method="dialog">
    <h2>Nouvelle discussion</h2>
    <label>Nom<input class="field" id="nc-name" placeholder="ex. refonte-parser" maxlength="60" autocomplete="off"></label>
    <label>Dossier
      <div class="seg" id="nc-mode">
        <button type="button" data-v="worktree" class="${mode === "worktree" ? "on" : ""}">Worktree isolé</button>
        <button type="button" data-v="project" class="${mode === "project" ? "on" : ""}">Dossier du projet</button>
      </div>
    </label>
    <label>Message</label>
    <div class="nc-msg"><textarea class="field area" id="nc-text" rows="4" placeholder="Que doit faire Claude ? (/ pour les commandes)"></textarea></div>
    <label>Modèle<span id="nc-model" class="nc-model"></span></label>
    <label class="check"><input type="checkbox" id="nc-bypass" checked> Tout autoriser (bypass)</label>
    <div class="muted small">${slotChip(repo.slot)} ${esc(repo.name)}</div>
    <div class="foot">
      <button class="btn" type="button" id="nc-term" title="Ouvrir dans Terminal">${icon("terminal")}</button>
      <span style="flex:1"></span>
      <button class="btn" value="cancel" formnovalidate>Annuler</button>
      <button class="btn primary" value="go" id="nc-go">Lancer</button>
    </div>
  </form>`;
  document.body.append(dlg);
  // Commandes `/` du projet (la discussion n'existe pas encore) : branchées avant le reste.
  attachSlash($("#nc-text", dlg), () => ({ repo: repo.id }), $(".nc-msg", dlg));
  attachImprove($("#nc-text", dlg), () => ({ repo_id: repo.id }));
  let model = "";
  modelPicker($("#nc-model", dlg), repo.slot, "", (v) => { model = v; });
  const nameEl = $("#nc-name", dlg);
  const syncName = () => {
    nameEl.required = mode === "worktree";
    nameEl.closest("label").style.opacity = mode === "worktree" ? 1 : 0.55;
  };
  syncName();
  $$("#nc-mode button", dlg).forEach((b) => b.addEventListener("click", () => {
    mode = b.dataset.v;
    $$("#nc-mode button", dlg).forEach((x) => x.classList.toggle("on", x === b));
    syncName();
  }));
  $("#nc-term", dlg).addEventListener("click", async () => {
    try {
      await api("/api/launch", { method: "POST", json: { repo_id: repo.id } });
      toast("Terminal ouvert.");
      dlg.close("cancel");
    } catch (e) { toast(e.message, true); }
  });
  $("#nc-go", dlg).addEventListener("click", async (e) => {
    e.preventDefault();
    const text = $("#nc-text", dlg).value.trim();
    const name = nameEl.value.trim();
    if (mode === "worktree" && !name) return nameEl.focus();
    if (!text) return $("#nc-text", dlg).focus();
    e.target.disabled = true;
    try {
      const c = await api("/api/chats", { method: "POST", json: { repo_id: repo.id, name, mode, bypass: $("#nc-bypass", dlg).checked, text, model: model || null } });
      dlg.close("go");
      onStarted(c);
    } catch (err) { toast(err.message, true); e.target.disabled = false; }
  });
  dlg.addEventListener("close", () => dlg.remove());
  dlg.showModal();
  nameEl.focus();
}

// ------------------------------------------------------------ composeur
// `draftKey` : le texte non envoyé est gardé (par discussion) et retrouvé au retour.
export function composer(placeholder, onSend, { draftKey = "", onStop = null, onRecall = null, improve = null, schedule = null, prompts = null, slash = null, btw = null, bash = null } = {}) {
  const el = document.createElement("div");
  el.className = "composer";
  el.innerHTML = `<div class="attach-row" hidden></div>
    <input type="file" accept="image/png,image/jpeg,image/gif,image/webp" multiple hidden>
    <textarea rows="1" placeholder="${esc(placeholder)}"></textarea>
    ${onStop ? `<button class="stop-btn" type="button" data-stop disabled title="Arrêter Claude" aria-label="Arrêter Claude"><span></span></button>` : ""}
    <button class="btn primary" type="button" data-send>Envoyer</button>
    <div class="composer-foot">
      <button class="icon-btn attach-btn" type="button" title="Joindre une image" aria-label="Joindre une image">${icon("clip")}</button>
      <span class="model-slot"></span>
      ${schedule ? `<button class="icon-btn sched-btn" type="button" title="Programmer l'envoi" aria-label="Programmer l'envoi">${icon("clock")}</button>` : ""}
      ${prompts ? `<button class="icon-btn foot-btn" type="button" data-psave title="Sauvegarder le prompt" aria-label="Sauvegarder le prompt">${icon("bookmark")}</button>
        <button class="icon-btn foot-btn" type="button" data-papply title="Appliquer un prompt" aria-label="Appliquer un prompt">${icon("list")}</button>` : ""}
      ${btw ? `<button class="btw-btn" type="button" data-btw title="Question annexe (/btw) : sans modifier la conversation ni interrompre l'agent">BTW</button>` : ""}
    </div>`;
  const ta = $("textarea", el);
  // Commandes `/` : branchées AVANT l'écoute d'Entrée, qui les complète au lieu d'envoyer.
  if (slash) attachSlash(ta, slash, el);
  // Images jointes (collées, déposées ou choisies), envoyées avec le message.
  // Pièces jointes : images et éléments de page. Chacune a son repère dans le texte
  // (`[Image #1]`, `[Élément #1]`), inséré au curseur ; la retirer retire son repère.
  let images = [], elements = [], imgN = 0, elN = 0;
  const row = $(".attach-row", el);
  const insertAt = (text) => {
    const a = ta.selectionStart ?? ta.value.length, b = ta.selectionEnd ?? a;
    const before = ta.value.slice(0, a), after = ta.value.slice(b);
    const pre = before && !/\s$/.test(before) ? " " : "", post = after && !/^\s/.test(after) ? " " : "";
    ta.value = before + pre + text + post + after;
    const at = (before + pre + text + post).length;
    ta.setSelectionRange(at, at);
    save(); grow();
  };
  const dropToken = (tok) => { ta.value = ta.value.replace(tok, "").replace(/ {2,}/g, " "); save(); grow(); };
  const drawRow = () => {
    row.hidden = !images.length && !elements.length;
    row.innerHTML = images.map((im, i) => `<div class="att" title="${esc(`Image #${im.n}`)}"><img src="data:${im.media_type};base64,${im.data}" alt=""><span class="att-n">#${im.n}</span><button type="button" data-rm="${i}" aria-label="Retirer">${icon("close")}</button></div>`).join("")
      + elements.map((e, i) => `<div class="att-el" title="${esc(e.info.selector)}"><span class="el-tag">Élément #${e.n}</span><code>${esc(e.info.short)}</code>${e.info.text ? `<span class="att-el-t">${esc(e.info.text.slice(0, 40))}</span>` : ""}<button type="button" data-rm-el="${i}" aria-label="Retirer">${icon("close")}</button></div>`).join("");
    $$("[data-rm]", row).forEach((b) => b.addEventListener("click", () => { const [im] = images.splice(+b.dataset.rm, 1); dropToken(`[Image #${im.n}]`); drawRow(); }));
    $$("[data-rm-el]", row).forEach((b) => b.addEventListener("click", () => { const [e] = elements.splice(+b.dataset.rmEl, 1); dropToken(token(e.n)); drawRow(); }));
  };
  const addFiles = (files) => {
    for (const f of files) {
      if (!/^image\/(png|jpeg|gif|webp)$/.test(f.type)) { toast("Seules les images PNG, JPEG, GIF ou WebP peuvent être jointes.", true); continue; }
      if (f.size > 5 * 1024 * 1024) { toast("Image trop lourde (5 Mo au plus).", true); continue; }
      const rd = new FileReader();
      rd.onload = () => { const n = ++imgN; images.push({ media_type: f.type, data: String(rd.result).split(",")[1], n }); insertAt(`[Image #${n}]`); drawRow(); };
      rd.readAsDataURL(f);
    }
  };
  const fileInput = $("input[type=file]", el);
  $(".attach-btn", el).addEventListener("click", () => fileInput.click());
  fileInput.addEventListener("change", () => { addFiles(fileInput.files); fileInput.value = ""; });
  ta.addEventListener("paste", (e) => {
    const files = [...(e.clipboardData?.files || [])].filter((f) => f.type.startsWith("image/"));
    if (files.length) { e.preventDefault(); addFiles(files); }
  });
  el.addEventListener("dragover", (e) => { e.preventDefault(); el.classList.add("drop"); });
  el.addEventListener("dragleave", () => el.classList.remove("drop"));
  el.addEventListener("drop", (e) => { e.preventDefault(); el.classList.remove("drop"); addFiles(e.dataTransfer.files); });
  const key = draftKey ? "drifty.draft." + draftKey : "";
  try { if (key) ta.value = localStorage.getItem(key) || ""; } catch {}
  if (bash && ta.value.startsWith("!")) el.classList.add("bash-mode");
  const save = () => { try { if (key) ta.value ? localStorage.setItem(key, ta.value) : localStorage.removeItem(key); } catch {} };
  // Hauteur = contenu + bordure (sinon il manque 2 px et une barre de défilement apparaît pour rien).
  // La barre ne s'affiche qu'au-delà de la hauteur maximale ; l'étoile se décale alors pour ne pas la couvrir.
  const MAX_H = 220;
  const grow = () => {
    ta.style.height = "auto";
    const full = ta.scrollHeight + (ta.offsetHeight - ta.clientHeight);
    ta.style.height = Math.min(full, MAX_H) + "px";
    ta.style.overflowY = full > MAX_H ? "auto" : "hidden";
    ta.parentElement?.classList.toggle("scrolling", full > MAX_H);
  };
  // `!` en tête (si la discussion le permet) : commande shell, comme dans le terminal.
  const isBash = () => !!bash && ta.value.startsWith("!");
  const markBash = () => el.classList.toggle("bash-mode", isBash());
  const send = async () => {
    const t = ta.value.trim();
    if (!t && !images.length && !elements.length) return;
    if (isBash()) {
      const cmd = ta.value.slice(1).trim();
      if (!cmd) return;
      ta.value = ""; save(); grow(); markBash(); // la saisie se libère : la commande tourne dans le fil
      bash(cmd).catch((e) => toast(e.message, true));
      return;
    }
    ta.disabled = true;
    try {
      await onSend(withElements(t, elements), images.map(({ media_type, data }) => ({ media_type, data })));
      ta.value = ""; images = []; elements = []; imgN = elN = 0; drawRow(); save(); grow();
    } catch (e) { toast(e.message, true); }
    ta.disabled = false;
    ta.focus();
  };
  ta.addEventListener("input", () => { grow(); save(); markBash(); });
  ta.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); send(); }
    // ↑ dans un champ vide : reprend le dernier message en attente (comme dans le terminal).
    if (e.key === "ArrowUp" && !ta.value && onRecall) { e.preventDefault(); onRecall(); }
  });
  $("[data-send]", el).addEventListener("click", send);
  if (onStop) $("[data-stop]", el).addEventListener("click", onStop);
  if (improve) attachImprove(ta, improve);
  // Programmer l'envoi : date et heure, ou « au prochain reset » du compte.
  if (schedule) {
    $(".sched-btn", el).addEventListener("click", (e) => {
      const t = ta.value.trim();
      if (!t) { toast("Écris d'abord le message à programmer.", true); return ta.focus(); }
      schedulePop(e.currentTarget, schedule.slot, async (at) => {
        await schedule.create(t, at);
        ta.value = ""; save(); grow();
        toast(`Message programmé pour ${new Date(at).toLocaleString("fr-FR", { weekday: "long", day: "numeric", month: "long", hour: "2-digit", minute: "2-digit" })}.`);
      });
    });
  }
  // Prompts sauvegardés : enregistrer le texte saisi (avec un nom), ou en insérer un.
  if (prompts) {
    $("[data-psave]", el).addEventListener("click", (e) => {
      const body = ta.value.trim();
      if (!body) { toast("Écris d'abord le prompt à sauvegarder.", true); return ta.focus(); }
      promptSavePop(e.currentTarget, body, prompts.repoId?.());
    });
    $("[data-papply]", el).addEventListener("click", (e) => promptApplyPop(e.currentTarget, prompts.repoId?.(), (text) => {
      const at = ta.selectionStart ?? ta.value.length;
      ta.value = ta.value ? ta.value.slice(0, at) + text + ta.value.slice(ta.selectionEnd ?? at) : text;
      save(); grow(); ta.focus();
    }));
  }
  if (btw) $("[data-btw]", el).addEventListener("click", () => openBtw(el, {
    ...btw,
    insert: (text) => { const at = ta.selectionStart ?? ta.value.length; ta.value = ta.value.slice(0, at) + text + ta.value.slice(at); save(); grow(); ta.focus(); },
  }));
  /** Joint un élément de page (onglet navigateur) : pastille + repère au curseur. */
  el.attachElement = (info) => { const n = ++elN; elements.push({ n, info }); insertAt(token(n)); drawRow(); };
  el.addText = (text) => insertAt(text);
  // Hauteur recalculée quand la saisie redevient visible (texte inséré pendant qu'elle était cachée).
  new ResizeObserver(() => { if (el.offsetParent) grow(); }).observe(el);
  el.fill = (text) => { ta.value = text; save(); grow(); markBash(); ta.focus(); ta.setSelectionRange(text.length, text.length); };
  requestAnimationFrame(grow);
  return el;
}

// ------------------------------------------------------------ prompts sauvegardés
function popAt(anchor, cls, html) {
  document.querySelector(".ctx-pop")?.remove();
  const pop = document.createElement("div");
  pop.className = `ctx-pop ${cls}`;
  pop.innerHTML = html;
  document.body.append(pop);
  const r = anchor.getBoundingClientRect();
  pop.style.left = Math.min(r.left, innerWidth - 340) + "px";
  pop.style.top = Math.max(8, r.top - pop.offsetHeight - 8) + "px";
  const close = () => { pop.remove(); removeEventListener("mousedown", outside); };
  const outside = (e) => { if (!pop.contains(e.target) && !anchor.contains(e.target)) close(); };
  addEventListener("mousedown", outside);
  return [pop, close];
}

function promptSavePop(anchor, body, repoId) {
  const [pop, close] = popAt(anchor, "prompt-pop", `<b style="font-size:13px">Sauvegarder le prompt</b>
    <input class="field" placeholder="Nom du prompt" maxlength="80">
    ${repoId ? `<label class="check"><input type="checkbox"> Seulement pour ce projet</label>` : ""}
    <div class="prompt-preview">${esc(body.slice(0, 240))}${body.length > 240 ? "…" : ""}</div>
    <div class="foot"><button class="btn" type="button" data-cancel>Annuler</button><button class="btn primary" type="button" data-ok>Sauvegarder</button></div>`);
  const name = pop.querySelector("input.field");
  name.focus();
  const ok = async () => {
    if (!name.value.trim()) return name.focus();
    const only = pop.querySelector("input[type=checkbox]")?.checked;
    try {
      await api("/api/prompts", { method: "POST", json: { title: name.value.trim(), body, repo_id: only ? repoId : null } });
      toast(`Prompt « ${name.value.trim()} » sauvegardé.`);
      close();
    } catch (e) { toast(e.message, true); }
  };
  name.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); ok(); } });
  pop.querySelector("[data-ok]").addEventListener("click", ok);
  pop.querySelector("[data-cancel]").addEventListener("click", close);
}

async function promptApplyPop(anchor, repoId, insert) {
  const list = await api(`/api/prompts${repoId ? `?repo=${repoId}` : ""}`).catch(() => []);
  const [pop, close] = popAt(anchor, "prompt-pop", `<b style="font-size:13px">Appliquer un prompt</b>
    ${list.length ? `<input class="field" placeholder="Rechercher">` : ""}
    <div class="prompt-list">${list.length ? "" : `<div class="muted small">Aucun prompt sauvegardé. Écris-en un dans la saisie, puis « Sauvegarder le prompt ».</div>`}</div>`);
  const box = pop.querySelector(".prompt-list");
  const draw = (q = "") => {
    const hits = list.filter((p) => `${p.title} ${p.body}`.toLowerCase().includes(q.toLowerCase()));
    box.innerHTML = hits.map((p) => `<div class="prompt-item" data-id="${p.id}">
        <div class="pi-main"><b>${esc(p.title)}</b>${p.repo ? ` <span class="chip">${esc(p.repo)}</span>` : ""}<span class="pi-body">${esc(p.body.slice(0, 120))}</span></div>
        <button class="icon-btn" type="button" data-del="${p.id}" title="Supprimer">${icon("close")}</button>
      </div>`).join("") || (list.length ? `<div class="muted small">Aucun résultat.</div>` : box.innerHTML);
  };
  if (list.length) draw();
  pop.querySelector("input.field")?.addEventListener("input", (e) => draw(e.target.value));
  pop.querySelector("input.field")?.focus();
  box.addEventListener("click", async (e) => {
    const del = e.target.closest("[data-del]");
    if (del) {
      if (!(await confirmBox("Supprimer ce prompt ?", { ok: "Supprimer", danger: true }))) return;
      await api(`/api/prompts/${del.dataset.del}`, { method: "DELETE" }).catch((err) => toast(err.message, true));
      list.splice(list.findIndex((p) => p.id === +del.dataset.del), 1);
      return draw(pop.querySelector("input.field")?.value || "");
    }
    const item = e.target.closest(".prompt-item");
    if (!item) return;
    const p = list.find((x) => x.id === +item.dataset.id);
    insert(p.body);
    api(`/api/prompts/${p.id}/used`, { method: "POST" }).catch(() => {});
    close();
  });
}

// ------------------------------------------------------------ programmation d'un message
const pad = (n) => String(n).padStart(2, "0");
const localInput = (d) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;

function schedulePop(anchor, slot, onPick) {
  document.querySelector(".sched-pop")?.remove();
  const pop = document.createElement("div");
  pop.className = "ctx-pop sched-pop";
  const soon = new Date(Date.now() + 3600e3);
  soon.setSeconds(0, 0);
  pop.innerHTML = `<b style="font-size:13px">Programmer l'envoi</b>
    <input class="field" type="datetime-local" value="${localInput(soon)}" min="${localInput(new Date())}">
    <button class="btn" type="button" data-reset hidden></button>
    <div class="foot"><button class="btn" type="button" data-cancel>Annuler</button><button class="btn primary" type="button" data-ok>Programmer</button></div>`;
  document.body.append(pop);
  const r = anchor.getBoundingClientRect();
  pop.style.left = Math.min(r.left, innerWidth - 300) + "px";
  pop.style.top = Math.max(8, r.top - 190) + "px";
  const input = pop.querySelector("input");
  // Raccourci : prochain reset de la session 5 h du compte (+1 min).
  api("/api/usage").then((accs) => {
    const a = accs.find((x) => x.slot === slot) || accs[0];
    const at = a?.five_hour?.resets_at_ms;
    if (!at || at < Date.now()) return;
    const b = pop.querySelector("[data-reset]");
    b.hidden = false;
    b.textContent = `Au prochain reset 5 h (${new Date(at + 60e3).toLocaleTimeString("fr-FR", { hour: "2-digit", minute: "2-digit" })})`;
    b.addEventListener("click", () => (input.value = localInput(new Date(at + 60e3))));
  }).catch(() => {});
  const close = () => { pop.remove(); removeEventListener("mousedown", outside); };
  const outside = (e) => { if (!pop.contains(e.target) && !anchor.contains(e.target)) close(); };
  addEventListener("mousedown", outside);
  pop.querySelector("[data-cancel]").addEventListener("click", close);
  pop.querySelector("[data-ok]").addEventListener("click", async () => {
    const at = new Date(input.value).getTime();
    if (!at || at < Date.now() - 60e3) return toast("Choisis une date à venir.", true);
    try { await onPick(at); close(); } catch (e) { toast(e.message, true); }
  });
}

/** Messages programmés d'une conversation, affichés au-dessus de la saisie (annulables). */
export function scheduledStrip(comp, query) {
  const box = document.createElement("div");
  box.className = "sched-strip";
  comp.before(box);
  const draw = async () => {
    if (!document.body.contains(box)) return clearInterval(timer);
    const list = await api(`/api/scheduled?${query}`).catch(() => []);
    box.innerHTML = list.map((m) => `<div class="sched-item">${icon("clock")}<span class="when">${esc(new Date(m.at_ms).toLocaleString("fr-FR", { weekday: "short", day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" }))}</span><span class="txt">${esc(m.text)}</span><button class="link" type="button" data-cancel="${m.id}">Annuler</button></div>`).join("");
    box.querySelectorAll("[data-cancel]").forEach((b) => b.addEventListener("click", async () => {
      try { await api(`/api/scheduled/${b.dataset.cancel}`, { method: "DELETE" }); toast("Message programmé annulé."); draw(); } catch (e) { toast(e.message, true); }
    }));
  };
  const timer = setInterval(draw, 15000);
  draw();
  return draw;
}

// ------------------------------------------------------------ rendu d'un chat en direct
function inputSummary(name, input) {
  if (!input) return "";
  if (input.command) return `<pre>${esc(input.command)}</pre>`;
  if (name === "Edit" || name === "MultiEdit") {
    return `<div class="perm-file">${esc(input.file_path || "")}</div>` +
      (input.old_string != null ? `<pre class="diff"><span class="del">${esc(String(input.old_string).slice(0, 800))}</span>\n<span class="add">${esc(String(input.new_string ?? "").slice(0, 800))}</span></pre>` : "");
  }
  if (name === "Write") return `<div class="perm-file">${esc(input.file_path || "")}</div><pre>${esc(String(input.content || "").slice(0, 800))}</pre>`;
  if (input.file_path) return `<div class="perm-file">${esc(input.file_path)}</div>`;
  return `<pre>${esc(JSON.stringify(input, null, 2).slice(0, 800))}</pre>`;
}

// ------------------------------------------------------------ « ✻ Hatching… (3m 34s · ↓ 10.9k tokens) »
const GLYPHS = ["·", "✢", "✳", "✶", "✻", "✽", "✻", "✶", "✳", "✢"];
const VERBS = ["Hatching", "Pondering", "Brewing", "Conjuring", "Percolating", "Simmering", "Noodling", "Tinkering",
  "Musing", "Cogitating", "Forging", "Whirring", "Mulling", "Crafting", "Scheming", "Churning"];
const compact = (n) => (n >= 1000 ? (n / 1000).toFixed(1).replace(/\.0$/, "") + "k" : String(n));
const elapsed = (ms) => {
  const s = Math.floor(ms / 1000);
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`;
};

/** Barre de conversation « ███████░░░ 77% », du vert au rouge. Repère réglable :
 *  un nombre de tokens à 100 %, ou un budget en dollars. */
let barCfg = null;
export async function loadBarCfg() {
  if (!barCfg) barCfg = await api("/api/settings/context-bar").catch(() => ({ mode: "tokens", tokens_cap: 1e9, budget_usd: 50 }));
  return barCfg;
}
export function convBar(total, cost) {
  const cfg = barCfg || { mode: "tokens", tokens_cap: 1e9, budget_usd: 50 };
  const budget = cfg.mode === "budget";
  const pct = Math.max(0, Math.min(1, budget ? (cost || 0) / cfg.budget_usd : (total || 0) / cfg.tokens_cap));
  const filled = Math.round(pct * 10);
  const hue = Math.round(120 * (1 - pct)); // 120 = vert, 0 = rouge
  const title = budget
    ? `${usd(cost || 0)} dépensés sur un budget de ${usd(cfg.budget_usd)}`
    : `${tok(total || 0)} tokens utilisés sur ${tok(cfg.tokens_cap)}`;
  return `<button type="button" class="ctxbar" data-ctxbar title="${title} (cliquer pour régler)"><span style="color:hsl(${hue} 75% 52%)">${"█".repeat(filled)}</span><span class="rest">${"░".repeat(10 - filled)}</span> ${Math.round(pct * 100)}%${budget ? ` <span class="muted">${usd(cost || 0)} / ${usd(cfg.budget_usd)}</span>` : ""}</button>`;
}

/** Réglage de la barre : petit panneau sous la barre ; `onSaved` redessine. */
export function barSettings(anchor, onSaved) {
  document.querySelector(".ctx-pop")?.remove();
  const cfg = barCfg || { mode: "tokens", tokens_cap: 1e9, budget_usd: 50 };
  const pop = document.createElement("div");
  pop.className = "ctx-pop";
  pop.innerHTML = `
    <div class="seg" data-mode><button type="button" data-v="tokens" class="${cfg.mode === "tokens" ? "on" : ""}">Tokens</button><button type="button" data-v="budget" class="${cfg.mode === "budget" ? "on" : ""}">Budget</button></div>
    <label data-for="tokens">100 % à<input class="field" type="number" min="1000" step="1000000" value="${cfg.tokens_cap}"><span class="muted small">tokens</span></label>
    <label class="check" data-for="tokens"><input type="checkbox" data-auto-compact${cfg.auto_compact !== false ? " checked" : ""}> Auto-compact à 100 %</label>
    <label data-for="budget">100 % à<input class="field" type="number" min="0.01" step="1" value="${cfg.budget_usd}"><span class="muted small">$</span></label>
    <label class="check" data-for="budget"><input type="checkbox" data-stop-at${cfg.stop_at_budget !== false ? " checked" : ""}> Arrêter la discussion à 99 % du budget</label>
    <div class="foot"><button class="btn" type="button" data-cancel>Annuler</button><button class="btn primary" type="button" data-save>Enregistrer</button></div>`;
  document.body.append(pop);
  const r = anchor.getBoundingClientRect();
  pop.style.left = Math.min(r.left, innerWidth - 300) + "px";
  pop.style.top = r.bottom + 8 + "px";
  let mode = cfg.mode;
  const sync = () => pop.querySelectorAll("label[data-for]").forEach((l) => (l.hidden = l.dataset.for !== mode));
  sync();
  pop.querySelectorAll("[data-mode] button").forEach((b) => b.addEventListener("click", () => {
    mode = b.dataset.v;
    pop.querySelectorAll("[data-mode] button").forEach((x) => x.classList.toggle("on", x === b));
    sync();
  }));
  const close = () => { pop.remove(); removeEventListener("mousedown", outside); };
  const outside = (e) => { if (!pop.contains(e.target) && e.target !== anchor && !anchor.contains(e.target)) close(); };
  addEventListener("mousedown", outside);
  pop.querySelector("[data-cancel]").addEventListener("click", close);
  pop.querySelector("[data-save]").addEventListener("click", async () => {
    const [tk, bd] = [...pop.querySelectorAll("input[type=number]")].map((i) => Number(i.value));
    const auto_compact = pop.querySelector("[data-auto-compact]").checked, stop_at_budget = pop.querySelector("[data-stop-at]").checked;
    try {
      barCfg = await api("/api/settings/context-bar", { method: "PUT", json: { mode, tokens_cap: tk, budget_usd: bd, auto_compact, stop_at_budget } });
      close();
      onSaved();
    } catch (e) { toast(e.message, true); }
  });
}

const toolLabel = (b) => {
  const i = b.input || {};
  return i.description || i.file_path || i.command || i.pattern || i.url || i.query || "";
};

export async function renderLive(box, chatId, onMeta) {
  closeStream();
  box.innerHTML = loader();
  const info = await api(`/api/chats/${chatId}`);
  box.innerHTML = `<div class="live">
      <div class="thread" id="lt"></div>
      <div class="working" id="lw" hidden><span class="glyph">✻</span><span class="verb"></span><span class="stats"></span></div>
    </div>`;
  const scroller = box;
  const thread = $("#lt", box);
  const working = $("#lw", box);
  const queued = new Map(); // id → bulle « en attente »
  const recall = async (qid) => {
    try {
      const r = await api(`/api/chats/${chatId}/queue/${encodeURIComponent(qid)}`, { method: "DELETE" });
      comp.fill(r.text);
    } catch (e) { toast(e.message, true); }
  };
  // Discussion arrêtée (processus fini, reprise refusée…) : écrire la reprend dans une nouvelle
  // discussion sur la même conversation, au lieu d'une saisie morte.
  let curStatus = info.status;
  const resumeStopped = async (body) => {
    const i = await api(`/api/chats/${chatId}`);
    const sid = i.session_id || i.resume_of;
    if (!sid) throw new Error("La discussion ne peut pas être reprise : elle s'est arrêtée avant d'avoir une conversation.");
    const c = await api("/api/chats", { method: "POST", json: { session_id: sid, bypass: i.bypass, model: i.model || null, title: i.name || null, ...body } });
    await api(`/api/chats/${chatId}`, { method: "DELETE" }).catch(() => {});
    location.hash = `#/conversations?c=${c.id}`;
    return c;
  };
  const comp = composer("Écris à Claude… (! pour une commande shell)", (t, images) => curStatus === "exited"
    ? resumeStopped({ text: t, images })
    : api(`/api/chats/${chatId}/send`, { method: "POST", json: { text: t, images } }), {
    draftKey: chatId,
    // `!commande` : tourne dans le dossier de la discussion, sortie au fil de l'eau ; elle part
    // avec le prochain message.
    bash: async (cmd) => {
      const target = curStatus === "exited" ? (await resumeStopped({})).id : chatId;
      await api(`/api/chats/${target}/bash`, { method: "POST", json: { text: cmd } });
    },
    improve: () => ({ chat_id: chatId }),
    prompts: { repoId: () => info.repo_id },
    slash: () => ({ chat: chatId }),
    btw: {
      target: () => ({ chat: chatId }),
      send: (text) => api(`/api/chats/${chatId}/send`, { method: "POST", json: { text } }).catch((e) => toast(e.message, true)),
      fork: async (q, a) => {
        const sid = (await api(`/api/chats/${chatId}`)).session_id;
        if (!sid) throw new Error("La discussion n'a pas encore de conversation à brancher : attends sa première réponse.");
        const c = await api("/api/chats", { method: "POST", json: { session_id: sid, fork: true, text: forkMessage(q, a), bypass: info.bypass } });
        location.hash = `#/conversations?c=${c.id}`;
      },
    },
    schedule: { slot: info.slot, create: async (text, at) => { await api("/api/scheduled", { method: "POST", json: { at_ms: at, chat_id: chatId, text } }); refreshSched(); } },
    onStop: () => api(`/api/chats/${chatId}/stop`, { method: "POST" }).catch((e) => toast(e.message, true)),
    onRecall: () => { const last = [...queued.keys()].pop(); if (last) recall(last); },
  });
  box.after(comp);
  const refreshSched = scheduledStrip(comp, `chat=${encodeURIComponent(chatId)}`);
  const stopBtn = $("[data-stop]", comp);
  const models = await loadModels(info.slot || "");
  const showModel = await modelPicker($(".model-slot", comp), info.slot, info.model, (v) =>
    api(`/api/chats/${chatId}/model`, { method: "POST", json: { model: v || "default" } }));

  const tools = new Map();
  const replyRaw = new Map(); // id de réponse → texte Markdown (pour copier / transférer)
  let curReply = null; // réponse en cours : se ferme au prochain message de l'utilisateur
  let live = null;
  let lastSeq = 0;
  let cost = info.cost_usd || 0;
  // Conversation reprise : son historique (avant l'ouverture dans l'app) s'affiche au-dessus.
  if (info.resume_of) {
    try {
      const past = (await api(`/api/sessions/${info.resume_of}/messages`)).filter((e) => !e.ts || e.ts < info.created);
      const recent = past.slice(-400);
      // Une « réponse » = tout ce que l'agent produit entre deux messages de l'utilisateur.
      let html = "", group = null;
      const flush = () => { if (group) html += `<div class="reply" data-r="${group.id}">${replyActions()}${group.html}</div>`; group = null; };
      for (const e of recent) {
        if (e.role === "user") {
          flush();
          const urls = (e.images || []).map((k) => `/api/sessions/${encodeURIComponent(info.resume_of)}/image?k=${encodeURIComponent(k)}`);
          html += `<div class="msg-user-wrap">${imageStrip(urls)}${e.text ? `<div class="msg-user">${userText(e.text)}</div>` : ""}</div>`;
          continue;
        }
        if (!group) { group = { id: "p" + replyRaw.size, html: "" }; replyRaw.set(group.id, ""); }
        if (e.role === "assistant") { group.html += `<div class="msg-ai">${md(e.text)}</div>`; replyRaw.set(group.id, replyRaw.get(group.id) + e.text + "\n\n"); }
        else if (e.role === "event") group.html += `<div class="event">${esc(e.text)}</div>`;
        else group.html += `<div class="tool-run done"><span class="dot"></span><b>${esc(e.tool || "")}</b><span class="lbl">${esc(e.text)}</span>${diffStat(e.diff)}</div>`;
      }
      flush();
      thread.insertAdjacentHTML("afterbegin", `${past.length > recent.length ? `<div class="event">${past.length - recent.length} éléments plus anciens non affichés</div>` : ""}${html}<div class="event">Reprise dans DRIFTy</div>`);
    } catch {}
  }
  // Gaspillage repéré en direct : un bandeau au-dessus de la saisie, avec l'action adaptée.
  const wasteBar = document.createElement("div");
  wasteBar.className = "waste-bar";
  wasteBar.hidden = true;
  comp.before(wasteBar);
  const say = (text) => api(`/api/chats/${chatId}/send`, { method: "POST", json: { text } }).then(() => toast("Consigne envoyée à Claude.")).catch((e) => toast(e.message, true));
  const showWaste = (w) => {
    if (live === undefined) return;
    const target = w.target ? ` <code>${esc(w.target)}</code>` : "";
    const [msg, label, act] =
      w.what === "loop" ? [`Boucle : ${esc(w.tool)}${target} a échoué ${w.count} fois de suite.`, "Interrompre et recadrer", async () => {
          await api(`/api/chats/${chatId}/stop`, { method: "POST" }).catch(() => {});
          comp.fill(`Tu relances ${w.target ? "`" + w.target + "`" : "la même commande"} qui échoue. Arrête-toi, lis l'erreur et corrige la cause avant de réessayer.`);
        }]
      : w.what === "big" ? [`Sortie énorme lue en entier : ${esc(w.tool)}${target} (${tok(w.bytes)}o).`, "Rappeler de filtrer",
          () => say("Pense à filtrer les sorties volumineuses (tail, grep, options silencieuses) au lieu de les lire en entier.")]
      : [`Échec évitable : ${esc(w.rule)}`, "Envoyer la consigne", () => say(w.rule)];
    wasteBar.innerHTML = `<span class="wb-icon">⚠</span><span class="wb-msg">${msg}</span><button class="btn" type="button" data-act>${label}</button><button class="icon-btn" type="button" data-x title="Ignorer">${icon("close")}</button>`;
    wasteBar.hidden = false;
    wasteBar.querySelector("[data-act]").addEventListener("click", async () => { wasteBar.hidden = true; await act(); });
    wasteBar.querySelector("[data-x]").addEventListener("click", () => (wasteBar.hidden = true));
  };
  // Proposition (jamais imposée) quand la limite du compte approche : modèle plus léger, ou l'autre compte.
  const tipBar = document.createElement("div");
  tipBar.className = "waste-bar tip-bar";
  tipBar.hidden = true;
  comp.before(tipBar);
  const checkLimits = async () => {
    if (!document.body.contains(tipBar)) return clearInterval(limitTimer);
    const accs = await api("/api/usage").catch(() => []);
    const mine = accs.find((a) => a.slot === info.slot);
    if (!mine) return;
    const h = mine.five_hour, w = mine.seven_day;
    const hot = h?.used_percentage >= 85 ? ["session 5 h", h] : w?.used_percentage >= 90 ? ["semaine", w] : null;
    if (!hot) { tipBar.hidden = true; return; }
    const key = `drifty.tip.${info.slot}.${hot[1].resets_at_ms}`;
    try { if (localStorage.getItem(key)) return; } catch {}
    const other = accs.find((a) => a.enabled && a.slot !== info.slot && (a.five_hour?.used_percentage ?? 0) < 50);
    const light = !/sonnet|haiku/.test(String(info.model || ""));
    if (!other && !light) return;
    tipBar.innerHTML = `<span class="wb-icon">↯</span><span class="wb-msg">${esc(mine.label)} : ${hot[0]} à ${Math.round(hot[1].used_percentage)} %.</span>
      ${light ? `<button class="btn" type="button" data-light>Passer à Sonnet</button>` : ""}
      ${other ? `<button class="btn" type="button" data-other>Continuer sur ${esc(other.label)}</button>` : ""}
      <button class="btn" type="button" data-later>Plus tard</button>`;
    tipBar.hidden = false;
    tipBar.querySelector("[data-later]").addEventListener("click", () => { try { localStorage.setItem(key, "1"); } catch {} tipBar.hidden = true; });
    tipBar.querySelector("[data-light]")?.addEventListener("click", async () => {
      try { await api(`/api/chats/${chatId}/model`, { method: "POST", json: { model: "sonnet" } }); tipBar.hidden = true; toast("Suite de la discussion sur Sonnet."); } catch (e) { toast(e.message, true); }
    });
    tipBar.querySelector("[data-other]")?.addEventListener("click", async () => {
      // Nouvelle discussion dans le même dossier, sur l'autre compte ; de quoi reprendre en brouillon.
      const last = [...replyRaw.values()].filter(Boolean).pop() || "";
      try {
        const c = await api("/api/chats", { method: "POST", json: { repo_id: info.repo_id, mode: "existing", worktree: info.worktree || "", slot: other.slot, title: `${info.name} (suite)`, bypass: info.bypass } });
        try { localStorage.setItem("drifty.draft." + c.id, `Je reprends un travail commencé dans une autre conversation (« ${info.name} »). Dernier point :\n\n${last.trim().slice(-3000)}\n\nContinue à partir de là.`); } catch {}
        location.hash = `#/conversations?c=${c.id}`;
      } catch (e) { toast(e.message, true); }
    });
  };
  const limitTimer = setInterval(checkLimits, 20000);
  checkLimits();
  const nearBottom = () => scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 140;
  const add = (html) => {
    const stick = nearBottom();
    thread.insertAdjacentHTML("beforeend", html);
    if (stick) scroller.scrollTop = scroller.scrollHeight;
    return thread.lastElementChild;
  };
  /** Ajoute dans la réponse en cours (créée au besoin). */
  const addReply = (html, raw = "") => {
    if (!curReply) {
      curReply = add(`<div class="reply" data-r="l${replyRaw.size}">${replyActions()}</div>`);
      replyRaw.set(curReply.dataset.r, "");
    }
    const stick = nearBottom();
    curReply.insertAdjacentHTML("beforeend", html);
    if (raw) replyRaw.set(curReply.dataset.r, replyRaw.get(curReply.dataset.r) + raw + "\n\n");
    if (stick) scroller.scrollTop = scroller.scrollHeight;
    return curReply.lastElementChild;
  };
  bindReplies(thread, (el) => replyRaw.get(el.dataset.r), async () => {
    const i = await api(`/api/chats/${chatId}`).catch(() => info);
    return { repoId: i.repo_id, title: i.name, selfChat: chatId, selfSession: i.session_id, worktree: i.worktree || "", rel: "" };
  });
  // Indicateur de tour : verbe tiré au hasard, étoile animée, durée et tokens reçus.
  const turn = { start: Date.now(), out: new Map(), timer: null, frame: 0 };
  let lastUserTs = 0;
  const glyph = $(".glyph", working), verb = $(".verb", working), stats = $(".stats", working);
  const drawTurn = () => {
    const down = [...turn.out.values()].reduce((a, b) => a + b, 0);
    stats.textContent = `(${elapsed(Date.now() - turn.start)} · ↓ ${compact(down)} tokens)`;
  };
  const startTurn = (ts) => {
    turn.start = ts || Date.now();
    turn.out.clear();
    verb.textContent = VERBS[Math.floor(Math.random() * VERBS.length)] + "…";
    clearInterval(turn.timer);
    turn.timer = setInterval(() => {
      if (!document.body.contains(working)) return clearInterval(turn.timer);
      glyph.textContent = GLYPHS[turn.frame++ % GLYPHS.length];
      if (turn.frame % 8 === 0) drawTurn();
    }, 120);
    drawTurn();
  };
  const setStatus = (s) => {
    if (s === "working" && working.hidden) startTurn(lastUserTs);
    if (s !== "working") clearInterval(turn.timer);
    working.hidden = s !== "working";
    stopBtn.disabled = s !== "working";
    curStatus = s;
    $("textarea", comp).placeholder = s === "exited" ? "Discussion arrêtée : écris pour la reprendre (! pour une commande shell)" : "Écris à Claude… (! pour une commande shell)";
    onMeta?.({ status: s, cost });
  };
  setStatus(info.status);
  await loadBarCfg();
  onMeta?.({ tokens: info.tokens_total, cost: info.cost_usd });

  let lastReported = 0; // dernier coût cumulé reçu (voir « result »)
  // Texte en cours d'écriture : rendu PAR LOTS, jamais à chaque fragment (refaire le Markdown de
  // toute la réponse à chaque fragment est quadratique : une longue réponse figeait la page et
  // lui faisait prendre des Go). L'intervalle suit le coût du dernier rendu (≤ ~20 % du temps) ;
  // fenêtre cachée : rien n'est rendu, la réponse finale remplace ce texte de toute façon.
  let liveTimer = 0, liveCost = 0;
  const drawLiveText = () => {
    liveTimer = 0;
    if (!live || document.hidden) return;
    const t0 = performance.now(), stick = nearBottom();
    // Les blocs déjà complets se rendent UNE fois (ajoutés au bout) ; seule la suite est refaite.
    if (!live._done) {
      live.innerHTML = `<div class="live-done"></div><div class="live-tail"></div>`;
      [live._done, live._tail, live._doneLen] = [live.firstChild, live.lastChild, 0];
    }
    const k = stableLength(live._text);
    if (k > live._doneLen) {
      live._done.insertAdjacentHTML("beforeend", md(live._text.slice(live._doneLen, k)));
      live._doneLen = k;
    }
    live._tail.innerHTML = md(live._text.slice(k));
    liveCost = performance.now() - t0;
    if (stick) scroller.scrollTop = scroller.scrollHeight;
  };
  const scheduleLive = () => { if (!liveTimer && !document.hidden) liveTimer = setTimeout(drawLiveText, Math.max(50, liveCost * 5)); };
  const asks = new Map(); // questions de Claude (AskUserQuestion) par demande, pour leur récapitulatif
  const bashEl = (id) => id && thread.querySelector(`[data-bash="${CSS.escape(id)}"]`);
  // Rendu d'un lot de sortie d'une commande `!` (voir « bash_chunk »).
  const flushBash = (pre) => {
    pre._timer = 0;
    if (!pre.isConnected || !pre._pending) return;
    const stick = nearBottom(), end = pre.scrollHeight - pre.scrollTop - pre.clientHeight < 20;
    pre.hidden = false;
    pre.textContent = appendTail(pre.textContent, pre._pending);
    pre._pending = "";
    // Mot de passe demandé (sudo…) : le champ d'entrée se masque et prend la main.
    const inp = pre.closest("[data-bash]")?.querySelector(".bash-in input");
    if (inp && inp.type !== "password" && asksPassword(pre.textContent)) { inp.type = "password"; inp.placeholder = "Mot de passe : envoyé à la commande seulement, jamais gardé"; inp.focus(); }
    if (end) pre.scrollTop = pre.scrollHeight;
    if (stick) scroller.scrollTop = scroller.scrollHeight;
  };
  // Entrée d'une commande `!` en cours (réponse, mot de passe) : envoyée, puis le champ se vide.
  thread.addEventListener("submit", (e) => {
    const f = e.target.closest(".bash-in");
    if (!f) return;
    e.preventDefault();
    const inp = f.querySelector("input"), line = inp.value;
    inp.value = "";
    api(`/api/chats/${chatId}/bash/${encodeURIComponent(f.closest("[data-bash]").dataset.bash)}/input`, { method: "POST", json: { text: line, secret: inp.type === "password" } }).catch((err) => toast(err.message, true));
    if (inp.type === "password") { inp.type = "text"; inp.placeholder = "Entrée pour la commande (Entrée pour envoyer)"; }
  });
  // « Arrêter » d'une commande `!` en cours.
  thread.addEventListener("click", (e) => {
    const b = e.target.closest(".bash-stop");
    if (!b) return;
    b.disabled = true;
    api(`/api/chats/${chatId}/bash/${encodeURIComponent(b.closest("[data-bash]").dataset.bash)}/stop`, { method: "POST" }).catch((err) => { toast(err.message, true); b.disabled = false; });
  });
  const handle = (ev, at) => {
    const t = ev.type;
    if (t === "drifty") {
      if (ev.kind === "waste") showWaste(ev);
      else if (ev.kind === "tokens") onMeta?.({ tokens: ev.total, cost: ev.cost });
      else if (ev.kind === "model") { showModel(ev.model); add(`<div class="event">Modèle : ${esc(modelLabel(ev.model, models))}</div>`); }
      else if (ev.kind === "diff") tools.get(ev.tool_id)?.insertAdjacentHTML("beforeend", diffStat([ev.added, ev.removed]));
      else if (ev.kind === "user_sent") {
        lastUserTs = ev.ts || Date.now(); // au rejeu, la durée du tour part du vrai envoi
        curReply = null;
        const urls = (ev.images || []).map((n) => `/api/chats/${chatId}/image/${n}`);
        add(`<div class="msg-user-wrap">${imageStrip(urls)}${ev.text ? `<div class="msg-user">${userText(ev.text)}</div>` : ""}</div>`);
      }
      else if (ev.kind === "queued") {
        const el = add(`<div class="msg-user queued" data-q="${esc(ev.id)}"><div>${userText(ev.text)}${ev.image_count ? ` <span class="muted">(+${ev.image_count} image${ev.image_count > 1 ? "s" : ""})</span>` : ""}</div><div class="q-foot"><span>En attente</span><button class="link" type="button" data-edit>Modifier</button><button class="link" type="button" data-cancel title="Retirer ce message de la file : il ne sera pas envoyé">Annuler</button><button class="link" type="button" data-now title="Claude le lit dès sa prochaine étape, sans interrompre son travail">Envoyer maintenant</button></div></div>`);
        el.querySelector("[data-edit]").addEventListener("click", () => recall(ev.id));
        // Annuler : le message quitte la file sans revenir dans la saisie.
        el.querySelector("[data-cancel]").addEventListener("click", (e) => {
          e.currentTarget.disabled = true;
          api(`/api/chats/${chatId}/queue/${encodeURIComponent(ev.id)}`, { method: "DELETE" }).then(() => toast("Message annulé.")).catch((err) => { toast(err.message, true); e.currentTarget.disabled = false; });
        });
        el.querySelector("[data-now]").addEventListener("click", (e) => {
          e.currentTarget.disabled = true;
          api(`/api/chats/${chatId}/queue/${encodeURIComponent(ev.id)}/now`, { method: "POST" }).catch((err) => { toast(err.message, true); e.currentTarget.disabled = false; });
        });
        queued.set(ev.id, el);
      } else if (ev.kind === "unqueued") {
        queued.get(ev.id)?.remove();
        queued.delete(ev.id);
      }
      else if (ev.kind === "bash_start") { if (!bashEl(ev.id)) add(bashRow(ev, true)); }
      else if (ev.kind === "bash_chunk") {
        const pre = bashEl(ev.id)?.querySelector(ev.stream === "out" ? "pre.out" : "pre.err");
        if (pre) {
          // Par lots (une sortie rapide arrive en milliers de morceaux) : ajout au tampon, rendu
          // au plus tous les 100 ms.
          pre._pending = (pre._pending || "") + ev.text;
          if (!pre._timer) pre._timer = setTimeout(() => flushBash(pre), 100);
        }
      }
      else if (ev.kind === "bash") { const el = bashEl(ev.id); if (el) el.outerHTML = bashRow(ev); else add(bashRow(ev)); }
      else if (ev.kind === "status") setStatus(ev.status);
      else if (ev.kind === "notice") add(`<div class="event notice">${esc(ev.text)}</div>`);
      else if (ev.kind === "setup") add(setupRow(ev));
      else if (ev.kind === "permission_resolved") {
        const card = thread.querySelector(`[data-perm="${CSS.escape(ev.request_id)}"]`);
        if (card?.classList.contains("plan")) { card.classList.add(ev.allow ? "ok" : "no"); card.removeAttribute("tabindex"); card.querySelector(".plan-actions").outerHTML = `<div class="plan-done">${esc(planOutcome(ev, planChoices(info.bypass)))}</div>`; }
        else if (card?.classList.contains("ask")) { card.classList.add(ev.allow ? "ok" : "no"); card.removeAttribute("tabindex"); card.innerHTML = askResolved(asks.get(ev.request_id), ev); }
        else if (card) { card.classList.add(ev.allow ? "ok" : "no"); card.querySelector(".perm-actions").innerHTML = `<span class="small">${ev.allow ? "Autorisé" : "Refusé"}</span>`; }
      } else if (ev.kind === "exit") add(`<div class="event">Session terminée${ev.code ? ` (code ${ev.code})` : ""} : écris pour la reprendre</div>`);
      else if (ev.kind === "stderr" && /error|erreur/i.test(ev.text)) add(`<div class="event err">${esc(ev.text)}</div>`);
      return;
    }
    if (t === "stream_event") {
      const e = ev.event || {};
      if (e.type === "message_start" && e.message?.id) { turn.cur = e.message.id; turn.out.set(turn.cur, e.message.usage?.output_tokens || 0); }
      if (e.type === "message_delta" && e.usage?.output_tokens != null && turn.cur) { turn.out.set(turn.cur, e.usage.output_tokens); drawTurn(); }
      if (e.type === "content_block_delta" && e.delta?.type === "text_delta") {
        if (!live) { live = addReply(`<div class="msg-ai streaming"></div>`); live._text = ""; }
        live._text += e.delta.text;
        scheduleLive();
      }
      return;
    }
    if (t === "system" && ev.subtype === "init" && ev.model) { showModel(ev.model); return; }
    if (t === "assistant") {
      for (const b of ev.message?.content || []) {
        if (b.type === "text" && b.text?.trim()) {
          if (live) { live.remove(); live = null; }
          addReply(`<div class="msg-ai">${md(b.text)}</div>`, b.text);
        } else if (b.type === "tool_use") {
          if (live) { live.remove(); live = null; }
          const el = addReply(`<div class="tool-run" data-tool="${esc(b.id)}"><span class="dot"></span><b>${esc(b.name)}</b><span class="lbl">${esc(toolLabel(b))}</span></div>`);
          tools.set(b.id, el);
        }
      }
      return;
    }
    if (t === "user") {
      for (const b of ev.message?.content || []) {
        if (b.type === "tool_result") tools.get(b.tool_use_id)?.classList.add(b.is_error ? "fail" : "done");
      }
      return;
    }
    if (t === "control_request" && ev.request?.subtype === "can_use_tool" && ev.request.tool_name === "ExitPlanMode") {
      const decide = (body) => api(`/api/chats/${chatId}/permission`, { method: "POST", json: { request_id: ev.request_id, ...body } }).catch((e) => { toast(e.message, true); throw e; });
      const card = addReply(`<div class="perm plan" data-perm="${esc(ev.request_id)}"></div>`);
      mountPlan(card, ev.request, {
        choices: planChoices(info.bypass),
        approve: (mode) => decide({ allow: true, mode }),
        keep: (message) => decide({ allow: false, message }),
      });
      return;
    }
    if (t === "control_request" && ev.request?.subtype === "can_use_tool" && ev.request.tool_name === "AskUserQuestion") {
      const decide = (body) => api(`/api/chats/${chatId}/permission`, { method: "POST", json: { request_id: ev.request_id, ...body } }).catch((e) => { toast(e.message, true); throw e; });
      const card = addReply(`<div class="perm ask" data-perm="${esc(ev.request_id)}"></div>`);
      asks.set(ev.request_id, ev.request);
      mountAsk(card, ev.request, {
        answer: (answers) => decide({ allow: true, answers }),
        refuse: (message) => decide({ allow: false, message }),
        chat: () => $("textarea", comp)?.focus(),
      });
      return;
    }
    if (t === "control_request" && ev.request?.subtype === "can_use_tool") {
      const r = ev.request;
      const card = addReply(`<div class="perm" data-perm="${esc(ev.request_id)}">
        <div class="perm-top"><b>${esc(r.display_name || r.tool_name)}</b><span>${esc(r.description || "")}</span></div>
        ${inputSummary(r.tool_name, r.input)}
        <div class="perm-actions"><button class="btn primary" data-allow>Autoriser</button><button class="btn" data-deny>Refuser</button></div>
      </div>`);
      const decide = (allow) => api(`/api/chats/${chatId}/permission`, { method: "POST", json: { request_id: ev.request_id, allow } }).catch((e) => toast(e.message, true));
      card.querySelector("[data-allow]").addEventListener("click", () => decide(true));
      card.querySelector("[data-deny]").addEventListener("click", () => decide(false));
      return;
    }
    if (t === "result") {
      if (!document.hidden) api(`/api/chats/${chatId}/seen`, { method: "POST" }).catch(() => {});
      if (live) { live.remove(); live = null; }
      const u = ev.usage || {};
      // `total_cost_usd` est CUMULÉ depuis le lancement (et, à la reprise, depuis le début de la
      // session) : le coût du tour est la différence avec le résultat précédent. Le total de la
      // discussion vient du serveur (événement « tokens »).
      const reported = ev.total_cost_usd || 0;
      const turnCost = reported >= lastReported ? reported - lastReported : reported;
      lastReported = reported;
      // Un résultat sans aucun tour (reprise, commande locale : 0 s, 0 token) n'est pas une réponse.
      const noop = ev.num_turns === 0 || (!ev.duration_ms && !u.output_tokens);
      if (!noop) add(`<div class="event" title="${esc(new Date(at || Date.now()).toLocaleString("fr-FR"))}">${ev.is_error ? "Erreur" : "Terminé"} ${esc(turnEnd(at, ev.duration_ms))} · ${usd(turnCost)} · ${tok((u.output_tokens || 0))} tokens écrits</div>`);
      curReply = null;
    }
  };

  source = new EventSource(`/api/chats/${chatId}/events`);
  source.onmessage = (m) => {
    const { seq, ev, at } = JSON.parse(m.data);
    if (seq <= lastSeq) return;
    lastSeq = seq;
    handle(ev, at);
  };
  source.onerror = () => {}; // le navigateur se reconnecte seul ; le rejeu est dédoublonné par `seq`
  toBottom(scroller);
  jumpButton(scroller, scroller.parentElement);
  $("textarea", comp).focus();
}
