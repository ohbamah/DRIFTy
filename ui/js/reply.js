// Actions sur une réponse de l'agent (tout ce qu'il produit entre deux messages
// de l'utilisateur) : copier son texte, ou le transférer vers une autre
// conversation du même projet (worktree, puis conversation).
import { api, esc, icon, toast, $, $$ } from "./lib.js";
import { store, convsFor, ensureLoaded } from "./store.js";

/** Boutons translucides affichés au survol d'une réponse. */
export const replyActions = () => `<div class="reply-actions">
  <button type="button" data-copy title="Copier la réponse" aria-label="Copier la réponse">${icon("copy")}</button>
  <button type="button" data-transfer title="Transférer vers une autre conversation" aria-label="Transférer">${icon("forward")}</button>
</div>`;

/**
 * Branche les actions d'un fil. `rawOf(replyEl)` rend le texte Markdown de la réponse ;
 * `ctx()` rend { repoId, title, selfSession, selfChat } de la conversation affichée.
 */
export function bindReplies(container, rawOf, ctx) {
  container.addEventListener("click", async (e) => {
    const btn = e.target.closest(".reply-actions button");
    if (!btn) return;
    const reply = btn.closest(".reply");
    const raw = (rawOf(reply) || "").trim();
    if (!raw) return toast("Cette réponse ne contient pas de texte.", true);
    if (btn.hasAttribute("data-copy")) {
      try { await navigator.clipboard.writeText(raw); toast("Réponse copiée."); } catch { toast("Copie impossible.", true); }
    } else {
      transferDialog(raw, await ctx());
    }
  });
}

/** Le brouillon d'une conversation (même clé que la barre de saisie). */
const draftKey = (target) => "claudius.draft." + (target.chat ? target.chat : "s:" + target.session);

async function transferDialog(raw, ctx) {
  await ensureLoaded();
  const repo = store.repos.find((r) => r.id === ctx.repoId);
  if (!repo) return toast("Le projet de cette conversation est inconnu : lance une analyse.", true);
  const groups = repo.groups.length ? repo.groups : [{ worktree: "", rel: "" }];
  const label = (g) => (g.worktree || g.rel ? [g.worktree, g.rel].filter(Boolean).join(" / ") : `${repo.root.split("/").pop()} (primary)`);
  const convs = await convsFor(repo.id);
  const live = store.chats.filter((c) => c.repo_id === repo.id && c.status !== "exited");

  const dlg = document.createElement("dialog");
  dlg.innerHTML = `<form class="dlg" method="dialog">
    <h2>Transférer la réponse</h2>
    <label>Worktree<select class="field" id="tf-wt">${groups.map((g, i) => `<option value="${i}">${esc(label(g))}</option>`).join("")}<option value="__new">Nouveau worktree…</option></select></label>
    <input class="field" id="tf-wt-name" placeholder="Nom du worktree (ex. refonte-parser)" maxlength="60" hidden>
    <label>Conversation<select class="field" id="tf-conv"></select></label>
    <input class="field" id="tf-conv-name" placeholder="Nom de la conversation" maxlength="80" hidden>
    <div class="tf-preview">${esc(raw.slice(0, 400))}${raw.length > 400 ? "…" : ""}</div>
    <label class="check"><input type="checkbox" id="tf-send"> Envoyer tout de suite</label>
    <div class="foot"><button class="btn" value="cancel" formnovalidate>Annuler</button><button class="btn primary" value="go" id="tf-go">Transférer</button></div>
  </form>`;
  document.body.append(dlg);
  const wt = $("#tf-wt", dlg), conv = $("#tf-conv", dlg);
  // Par défaut : le worktree de la conversation d'origine.
  const own = groups.findIndex((g) => g.worktree === (ctx.worktree || "") && g.rel === (ctx.rel || ""));
  if (own >= 0) wt.value = String(own);
  const wtName = $("#tf-wt-name", dlg), convName = $("#tf-conv-name", dlg);
  convName.value = `Transfert : ${ctx.title}`.slice(0, 80);
  const syncNames = () => {
    wtName.hidden = wt.value !== "__new";
    convName.hidden = conv.value !== "__new";
    $("#tf-go", dlg).disabled = !conv.value;
  };
  const fill = () => {
    if (wt.value === "__new") {
      // Nouveau worktree : la conversation est forcément nouvelle.
      conv.innerHTML = `<option value="__new">Nouvelle conversation…</option>`;
      syncNames();
      wtName.focus();
      return;
    }
    const g = groups[+wt.value];
    const bySession = new Map(live.filter((c) => c.session_id).map((c) => [c.session_id, c]));
    const fresh = live.filter((c) => !convs.some((x) => x.id === c.session_id) && (c.worktree || "") === g.worktree && !g.rel);
    const opts = [
      ...fresh.map((c) => ({ v: "c:" + c.id, t: `● ${c.name}` })),
      ...convs.filter((c) => c.worktree === g.worktree && c.rel === g.rel).map((c) => {
        const l = bySession.get(c.id);
        return { v: l ? "c:" + l.id : "s:" + c.id, t: `${l ? "● " : ""}${c.title || c.first_prompt || c.id.slice(0, 8)}` };
      }),
    ].filter((o) => o.v !== "s:" + ctx.selfSession && o.v !== "c:" + ctx.selfChat);
    conv.innerHTML = opts.map((o) => `<option value="${esc(o.v)}">${esc(o.t.slice(0, 90))}</option>`).join("") +
      `<option value="__new"${opts.length ? "" : " selected"}>Nouvelle conversation…</option>`;
    syncNames();
  };
  wt.addEventListener("change", fill);
  conv.addEventListener("change", () => { syncNames(); if (conv.value === "__new") convName.select(); });
  fill();

  $("#tf-go", dlg).addEventListener("click", async (e) => {
    e.preventDefault();
    const text = `Réponse transférée depuis « ${ctx.title} » :\n\n${raw}`;
    const send = $("#tf-send", dlg).checked;
    // Nouvelle conversation (dans un worktree existant ou nouveau) : une discussion de l'app.
    if (conv.value === "__new") {
      const newWt = wt.value === "__new";
      const name = wtName.value.trim();
      if (newWt && !name) return wtName.focus();
      const g = newWt ? null : groups[+wt.value];
      const body = {
        repo_id: repo.id, bypass: true, title: convName.value.trim() || null, text: send ? text : null,
        ...(newWt ? { mode: "worktree", name } : !g.worktree && !g.rel ? { mode: "project" } : { mode: "existing", worktree: g.worktree }),
      };
      e.target.disabled = true;
      try {
        const chat = await api("/api/chats", { method: "POST", json: body });
        if (!send) try { localStorage.setItem("claudius.draft." + chat.id, text); } catch {}
        location.hash = `#/conversations?c=${chat.id}`;
        toast(send ? "Nouvelle conversation créée, réponse envoyée." : "Nouvelle conversation créée, réponse en brouillon.");
        dlg.close();
      } catch (err) { toast(err.message, true); e.target.disabled = false; }
      return;
    }
    const [kind, id] = conv.value.split(/:(.+)/);
    if (!id) return;
    const target = kind === "c" ? { chat: id } : { session: id };
    try {
      if (send) {
        if (target.chat) {
          await api(`/api/chats/${id}/send`, { method: "POST", json: { text } });
          location.hash = `#/conversations?c=${id}`;
        } else {
          const chat = await api("/api/chats", { method: "POST", json: { session_id: id, text, bypass: true } });
          location.hash = `#/conversations?c=${chat.id}`;
        }
        toast("Réponse transférée et envoyée.");
      } else {
        // Déposée en brouillon dans la saisie de la conversation cible, prête à relire.
        try { localStorage.setItem(draftKey(target), text); } catch {}
        location.hash = target.chat ? `#/conversations?c=${id}` : `#/conversations?s=${id}`;
        toast("Réponse déposée en brouillon dans la conversation.");
      }
      dlg.close();
    } catch (err) { toast(err.message, true); }
  });
  dlg.addEventListener("close", () => dlg.remove());
  dlg.showModal();
}
