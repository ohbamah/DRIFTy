// Détail d'une conversation : fil, analyse, ou discussion en direct.
import { api, esc, ago, usd, icon, slotChip, toast, loader, imageStrip, diffStat, $, $$ } from "../lib.js";
import { md } from "../md.js";
import { renderAnalysis } from "./analysis.js";
import { slide, moveTo } from "../slide.js";
import { renderLive, composer, closeStream, convBar, barSettings, loadBarCfg, modelPicker, scheduledStrip } from "./chat.js";
import { store, ensureLoaded, reload } from "../store.js";
import { replyActions, bindReplies } from "../reply.js";
import { openReview } from "../review.js";
import { openGithub } from "../github.js";
import { openJob } from "../run.js";
import { openRun } from "../run.js";
import { forkMessage } from "../btw.js";
import { mountTabs } from "../tabs.js";
import { userText } from "../elements.js";

// L'URL porte la sélection : #/conversations?s=<conversation> | c=<discussion en direct> &t=<onglet>
function nav(patch) {
  const q = new URLSearchParams(location.hash.split("?")[1] || "");
  for (const [k, v] of Object.entries(patch)) (v === null || v === undefined ? q.delete(k) : q.set(k, v));
  location.hash = "#/conversations?" + q.toString();
}

/** Zone principale : le détail de la conversation choisie dans la barre latérale. */
export async function show(root, params) {
  await ensureLoaded();
  if (!root.querySelector("#detail")) {
    root.innerHTML = `<div class="detail" id="detail"></div>`;
    if (detailKey.startsWith("chat|")) closeStream();
    detailKey = "";
  }
  const s = params.get("s"), c = params.get("c"), tab = params.get("t") || "conv";
  if (c) await renderChatDetail(c);
  else await renderDetail(s, tab);
}

export function refresh() { return reload(); }

// ------------------------------------------------------------ chat en direct
async function renderChatDetail(chatId) {
  const box = $("#detail");
  const key = "chat|" + chatId;
  if (detailKey === key) return;
  detailKey = key;
  closeStream();
  box.innerHTML = loader();
  let info;
  api(`/api/chats/${chatId}/seen`, { method: "POST" }).catch(() => {});
  try { info = await api(`/api/chats/${chatId}`); } catch (e) {
    if (detailKey !== key) return;
    detailKey = "";
    box.innerHTML = `<div class="empty"><div><b>Discussion introuvable</b>${esc(e.message)}</div></div>`;
    return;
  }
  if (detailKey !== key) return; // une autre conversation a été choisie entre-temps
  box.innerHTML = `
    <div class="detail-head">
      <div class="row">
        <button class="icon-btn back" data-back title="Conversations">${icon("back")}</button>
        <h1>${esc(info.name)}</h1>
        <button class="btn" id="chat-run" title="Tests et lancement du projet">▶ Commandes</button>
        <button class="btn" id="chat-review" title="Voir les changements du dossier">${icon("branch")}Changements</button>
        <button class="btn icon-only" id="chat-gh" title="GitHub : PR, CI, relectures">${icon("github")}</button>
        <button class="btn" id="chat-close" title="Arrêter et fermer la discussion">Fermer</button>
      </div>
      <div class="meta">${slotChip(info.slot)}${info.worktree ? `<span>${icon("branch").replace("<svg", '<svg style="width:13px;height:13px;vertical-align:-2px"')} ${esc(info.worktree)}</span>` : ""}${info.bypass ? `<span class="chip waste">Bypass</span>` : `<span class="chip">Autorisations demandées</span>`}<span id="chat-cost"></span><span id="chat-bar"></span></div>
      <div class="tabs" role="tablist"><button class="tab on" data-tab="conv" role="tab">Conversation</button></div>
    </div>
    ${info.setup_job ? `<div class="setup-strip" id="setup-strip"></div>` : ""}
    <div class="detail-body" id="detail-body"></div>`;
  $("[data-back]", box).addEventListener("click", () => document.body.classList.remove("sb-hidden"));
  if (info.setup_job) followSetup($("#setup-strip", box), info.setup_job, info.name);
  slide(box);
  mountTabs(box, $(".tabs", box), "c:" + chatId, { chat: chatId }, {
    send: (text) => api(`/api/chats/${chatId}/send`, { method: "POST", json: { text } }).catch((e) => toast(e.message, true)),
  });
  const sendToChat = (text) => api(`/api/chats/${chatId}/send`, { method: "POST", json: { text } }).catch((e) => toast(e.message, true));
  $("#chat-gh", box).addEventListener("click", () => openGithub({ chat: chatId }, info.name, { toClaude: sendToChat }));
  $("#chat-review", box).addEventListener("click", () => openReview({ chat: chatId }, info.name));
  $("#chat-run", box).addEventListener("click", () => openRun({ chat: chatId }, info.repo_id, info.name, {
    onResult: (text) => api(`/api/chats/${chatId}/send`, { method: "POST", json: { text } }).then(() => toast("Résultat envoyé à Claude.")).catch((e) => toast(e.message, true)),
  }));
  $("#chat-close", box).addEventListener("click", async () => {
    if (!confirm("Arrêter et fermer cette discussion ? Elle restera lisible après une analyse.")) return;
    try { await api(`/api/chats/${chatId}`, { method: "DELETE" }); closeStream(); detailKey = ""; location.hash = "#/conversations"; } catch (e) { toast(e.message, true); }
  });
  const bar = { tokens: info.tokens_total, cost: info.cost_usd };
  const drawBar = () => {
    $("#chat-bar").innerHTML = convBar(bar.tokens, bar.cost);
    $("#chat-bar [data-ctxbar]")?.addEventListener("click", (e) => barSettings(e.currentTarget, drawBar));
  };
  await renderLive($("#detail-body"), chatId, (m) => {
    if (m.cost != null) $("#chat-cost").textContent = m.cost ? usd(m.cost) : "";
    if (m.tokens != null) bar.tokens = m.tokens;
    if (m.cost != null) bar.cost = m.cost;
    if (m.tokens != null || m.cost != null) drawBar();
  });
}

// Conversation passée : un prompt va en brouillon dans sa saisie (rien ne tourne pour le recevoir).
function toDraft(sid, text) {
  try { localStorage.setItem("claudius.draft.s:" + sid, text); } catch {}
  const ta = document.querySelector(".composer textarea");
  if (ta) { ta.value = text; ta.dispatchEvent(new Event("input", { bubbles: true })); ta.focus(); }
}

// Mise en place d'un worktree neuf : état du script du projet, sortie consultable.
async function followSetup(el, jobId, name) {
  const show = (html, cls = "") => { el.className = "setup-strip " + cls; el.innerHTML = html + `<span style="flex:1"></span><button class="btn small" data-out>Voir la sortie</button>`; el.querySelector("[data-out]").addEventListener("click", () => openJob(jobId, "Mise en place — " + name)); };
  show(`<span class="spin" style="width:12px;height:12px;border-width:2px"></span> Mise en place du worktree (script du projet)… Ton premier message partira ensuite.`);
  for (;;) {
    const r = await api(`/api/run/${jobId}?from=999999999`).catch(() => null);
    if (!el.isConnected) return;
    if (!r) return show(`Sortie de la mise en place indisponible.`, "warn");
    if (r.done) return r.code === 0 ? show(`${icon("check")} Worktree prêt.`, "ok") : show(`⚠ La mise en place a échoué (code ${r.code ?? "?"}).`, "warn");
    await new Promise((res) => setTimeout(res, 1500));
  }
}

// ------------------------------------------------------------ détail
let detailKey = "";
async function renderDetail(sid, tab) {
  const box = $("#detail");
  const live = sid && store.chats.find((x) => x.session_id === sid && x.status !== "exited");
  if (live && tab !== "analyse") return nav({ c: live.id, s: null });
  if (detailKey.startsWith("chat|")) { closeStream(); detailKey = ""; }
  if (!sid) {
    detailKey = "";
    box.innerHTML = `<div class="empty"><div><b>Choisis une conversation</b>dans la barre de gauche.</div></div>`;
    return;
  }
  const key = sid + "|" + tab;
  if (key === detailKey) return;
  const sameConv = detailKey.split("|")[0] === sid && $("#detail-body");
  detailKey = key;
  if (sameConv) return renderBody($("#detail-body"), sid, tab);
  box.innerHTML = loader();
  let conv;
  try { conv = await api(`/api/sessions/${sid}`); } catch (e) {
    if (detailKey !== key) return;
    detailKey = "";
    box.innerHTML = `<div class="empty"><div><b>Conversation illisible</b>${esc(e.message)}<div style="margin-top:12px"><button class="btn" data-retry>Réessayer</button></div></div></div>`;
    $("[data-retry]", box).addEventListener("click", () => renderDetail(sid, tab));
    return;
  }
  if (detailKey !== key) return; // une autre conversation a été choisie entre-temps
  box.innerHTML = `
    <div class="detail-head">
      <div class="row">
        <button class="icon-btn back" data-back title="Conversations">${icon("back")}</button>
        <h1>${esc(conv.title || conv.first_prompt || sid)}</h1>
        <button class="btn" id="conv-run" title="Tests et lancement du projet">▶ Commandes</button>
        <button class="btn" id="conv-review" title="Voir les changements du dossier">${icon("branch")}Changements</button>
        <button class="btn icon-only" id="resume" title="Ouvrir dans Terminal">${icon("terminal")}</button>
        <button class="btn icon-only" id="conv-gh" title="GitHub : PR, CI, relectures">${icon("github")}</button>
      </div>
      <div class="meta">${slotChip(conv.slot)}<span>${ago(conv.last_ts)}</span>${conv.branch ? `<span>${icon("branch").replace("<svg", '<svg style="width:13px;height:13px;vertical-align:-2px"')} ${esc(conv.branch)}</span>` : ""}${conv.cost_usd != null ? `<span>${usd(conv.cost_usd)}</span>` : ""}<span id="conv-bar"></span></div>
      <div class="tabs" role="tablist">
        <button class="tab${tab === "conv" ? " on" : ""}" data-tab="conv" role="tab">Conversation</button>
        <button class="tab${tab === "analyse" ? " on" : ""}" data-tab="analyse" role="tab">Analyse</button>
      </div>
    </div>
    <div class="detail-body" id="detail-body"></div>`;
  slide(box);
  mountTabs(box, $(".tabs", box), "s:" + sid, { session: sid }, {});
  // Barre de conversation, même quand aucun agent ne tourne : totaux de la dernière analyse.
  (async () => {
    const [info] = await Promise.all([api(`/api/sessions/${sid}`), loadBarCfg()]);
    const el = $("#conv-bar", box);
    if (!el) return;
    const draw = () => {
      el.innerHTML = convBar(info.tokens_total, info.cost_usd);
      el.querySelector("[data-ctxbar]")?.addEventListener("click", (e) => barSettings(e.currentTarget, draw));
    };
    draw();
  })().catch(() => {});
  $$(".tab[data-tab]", box).forEach((b) => b.addEventListener("click", () => {
    if (b.classList.contains("on")) return;
    moveTo(b);
    nav({ t: b.dataset.tab === "conv" ? null : b.dataset.tab });
  }));
  $("[data-back]", box).addEventListener("click", () => document.body.classList.remove("sb-hidden"));
  $("#conv-review", box).addEventListener("click", () => openReview({ session: sid }, conv.title || ""));
  $("#conv-gh", box).addEventListener("click", () => openGithub({ session: sid }, conv.title || "", { toClaude: (text) => toDraft(sid, text) }));
  $("#conv-run", box).addEventListener("click", () => openRun({ session: sid }, conv.repo_id, conv.title || "", {
    onResult: (text) => { toDraft(sid, text); toast("Résultat déposé dans la saisie."); },
  }));
  $("#resume", box).addEventListener("click", async () => {
    try {
      await api("/api/launch", { method: "POST", json: { session_id: sid } });
      toast("Terminal ouvert.");
    } catch (e) { toast(e.message, true); }
  });
  await renderBody($("#detail-body"), sid, tab);
}

async function renderBody(body, sid, tab) {
  body.parentElement.querySelectorAll(":scope > .composer").forEach((x) => x.remove());
  try {
    if (tab === "analyse") await renderAnalysis(body, sid);
    else {
      await renderThread(body, sid);
      if (!body.isConnected || detailKey !== sid + "|conv") return;
      // Continuer ici : relance la conversation dans un chat de l'app (même dossier, même compte).
      let model = "", repoOfConv = null;
      const comp = composer("Continuer cette conversation…", async (text, images) => {
        const act = await api(`/api/sessions/${sid}/activity`).catch(() => ({}));
        if (act.last_write_ms_ago != null && act.last_write_ms_ago < 120000) {
          const secs = Math.round(act.last_write_ms_ago / 1000);
          const ok = confirm(`Cette conversation a été écrite il y a ${secs} s : elle est sans doute ouverte ailleurs (terminal ou autre). La reprendre ici fera travailler deux agents dans la même conversation. Continuer quand même ?`);
          if (!ok) throw new Error("Reprise annulée.");
        }
        const chat = await api("/api/chats", { method: "POST", json: { session_id: sid, text, images, bypass: true, model: model || null } });
        nav({ c: chat.id, s: null, t: null });
      }, {
        draftKey: "s:" + sid, onStop: () => {}, improve: () => ({ session_id: sid }), // stop affiché, grisé : rien ne tourne ici
        prompts: { repoId: () => repoOfConv },
        slash: () => ({ session: sid }),
        btw: {
          target: () => ({ session: sid }),
          fork: async (q, a) => {
            const c = await api("/api/chats", { method: "POST", json: { session_id: sid, fork: true, text: forkMessage(q, a), bypass: true, model: model || null } });
            nav({ c: c.id, s: null, t: null });
          },
        },
        schedule: { slot: null, create: async (text, at) => { await api("/api/scheduled", { method: "POST", json: { at_ms: at, session_id: sid, text, model: model || null } }); refreshSched(); } },
      });
      body.after(comp);
      const refreshSched = scheduledStrip(comp, `session=${encodeURIComponent(sid)}`);
      const info = await api(`/api/sessions/${sid}`).catch(() => ({}));
      repoOfConv = info.repo_id;
      modelPicker($(".model-slot", comp), info.slot, info.last_model, (v) => { model = v; });
    }
  } catch (e) {
    body.innerHTML = `<div class="inset err">${esc(e.message)}</div>`;
  }
}

const PAGE = 300;
async function renderThread(body, sid) {
  body.innerHTML = loader();
  const entries = await api(`/api/sessions/${sid}/messages`);
  if (!body.isConnected || detailKey !== sid + "|conv") return; // onglet ou conversation changés entre-temps
  // Les appels d'outil consécutifs se replient en un seul bloc.
  const blocks = [];
  for (const e of entries) {
    const last = blocks[blocks.length - 1];
    if (e.role === "tool" && last?.role === "tools") last.items.push(e);
    else if (e.role === "tool") blocks.push({ role: "tools", items: [e] });
    else blocks.push(e);
  }
  let shown = Math.min(PAGE, blocks.length);
  let raws = [];
  // Copier / transférer une réponse (contexte lu à la demande).
  bindReplies(body, (el) => raws[+el.dataset.r], async () => {
    const i = await api(`/api/sessions/${sid}`);
    return { repoId: i.repo_id, title: i.title || sid.slice(0, 8), selfSession: sid, worktree: i.worktree || "", rel: i.rel || "" };
  });
  const draw = () => {
    const slice = blocks.slice(blocks.length - shown);
    // Une « réponse » = tout ce que l'agent produit entre deux messages de l'utilisateur.
    raws = [];
    let html = "", group = null;
    const flush = () => { if (group) html += `<div class="reply" data-r="${raws.length - 1}">${replyActions()}${group}</div>`; group = null; };
    for (const b of slice) {
      if (b.role === "user") { flush(); html += block(b, sid); continue; }
      if (group === null) { group = ""; raws.push(""); }
      group += block(b, sid);
      if (b.role === "assistant") raws[raws.length - 1] += b.text + "\n\n";
    }
    flush();
    body.innerHTML = `<div class="thread">
      ${shown < blocks.length ? `<div style="text-align:center;margin-bottom:12px"><button class="btn" id="older">Plus anciens</button></div>` : ""}
      ${html || `<div class="muted">Aucun message lisible dans ce transcript.</div>`}
    </div>`;
    $("#older", body)?.addEventListener("click", () => { shown = Math.min(blocks.length, shown + PAGE); draw(); body.scrollTop = 0; });
  };
  draw();
  body.scrollTop = body.scrollHeight;
}

function block(b, sid) {
  if (b.role === "user") {
    const urls = (b.images || []).map((k) => `/api/sessions/${encodeURIComponent(sid)}/image?k=${encodeURIComponent(k)}`);
    return `<div class="msg-user-wrap">${imageStrip(urls)}${b.text ? `<div class="msg-user">${userText(b.text)}</div>` : ""}</div>`;
  }
  if (b.role === "assistant") return `<div class="msg-ai">${md(b.text)}</div>`;
  if (b.role === "event") return `<div class="event">${esc(b.text)}</div>`;
  const names = [...new Set(b.items.map((t) => t.tool))];
  const label = b.items.length === 1
    ? `${esc(b.items[0].tool)} <span style="opacity:.75">${esc(b.items[0].text)}</span>`
    : `${b.items.length} actions · ${esc(names.slice(0, 3).join(", "))}${names.length > 3 ? "…" : ""}`;
  const total = b.items.reduce((acc, t) => (t.diff ? [acc[0] + t.diff[0], acc[1] + t.diff[1]] : acc), [0, 0]);
  return `<details class="tools"><summary>${icon("chevron")}<span>${label}</span>${total[0] || total[1] ? diffStat(total) : ""}</summary>
    ${b.items.map((t) => `<div class="tool-line"><b>${esc(t.tool)}</b><span title="${esc(t.text)}">${esc(t.text)}</span>${diffStat(t.diff)}</div>`).join("")}
  </details>`;
}

