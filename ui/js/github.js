// Panneau GitHub d'une conversation, dessiné comme la page d'une PR sur GitHub :
// en-tête, relecteurs, fil (HTML rendu par GitHub, fils résolus repliés), boîte de
// fusion (relectures, checks, conflits, bouton de fusion), commentaire. Les actions
// simples lancent git / gh ; les complexes deviennent un prompt confié à Claude.
import { api, esc, icon, toast, loader, ago, $, $$ } from "./lib.js";

// ------------------------------------------------------------ icônes (traits, 24 × 24)
const P = {
  pr: '<circle cx="6" cy="5" r="2.2"/><circle cx="6" cy="19" r="2.2"/><circle cx="18" cy="19" r="2.2"/><path d="M6 7.2v9.6M18 16.8V10a3 3 0 0 0-3-3h-4.5M12.5 4.5 10 7l2.5 2.5"/>',
  merged: '<circle cx="6" cy="5" r="2.2"/><circle cx="6" cy="19" r="2.2"/><circle cx="18" cy="13" r="2.2"/><path d="M6 7.2v9.6M6.5 7.5c1 3 4.5 5.5 9.3 5.5"/>',
  prclosed: '<circle cx="6" cy="5" r="2.2"/><circle cx="6" cy="19" r="2.2"/><circle cx="18" cy="19" r="2.2"/><path d="M6 7.2v9.6M18 16.8V12M15.5 4.5l5 5M20.5 4.5l-5 5"/>',
  draft: '<circle cx="6" cy="5" r="2.2"/><circle cx="6" cy="19" r="2.2"/><circle cx="18" cy="19" r="2.2"/><path d="M6 7.2v9.6M18 16.8v-1.5M18 12v-1.5M18 7V5.5"/>',
  eye: '<path d="M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7S2 12 2 12Z"/><circle cx="12" cy="12" r="3"/>',
  comment: '<path d="M4 5h16a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1h-9l-5 4v-4H4a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1Z"/>',
  file: '<path d="M14 3H6.5A1.5 1.5 0 0 0 5 4.5v15A1.5 1.5 0 0 0 6.5 21h11a1.5 1.5 0 0 0 1.5-1.5V8Z"/><path d="M14 3v5h5"/>',
  alert: '<path d="M10.3 4.2 2.6 18a2 2 0 0 0 1.7 3h15.4a2 2 0 0 0 1.7-3L13.7 4.2a2 2 0 0 0-3.4 0Z"/><path d="M12 9.5v4.5M12 17.2v.1"/>',
  check: '<path d="m5 12.5 4.5 4.5L19 7.5"/>',
  x: '<path d="M6 6l12 12M18 6 6 18"/>',
  dot: '<circle cx="12" cy="12" r="4" fill="currentColor" stroke="none"/>',
  changes: '<path d="M12 4v8M8 8h8M8 17h8"/><rect x="3" y="2.5" width="18" height="19" rx="2"/>',
  gear: '<circle cx="12" cy="12" r="3"/><path d="M12 2v3M12 19v3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M2 12h3M19 12h3M4.9 19.1 7 17M17 7l2.1-2.1"/>',
  claude: '<path d="M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8Z"/>',
};
const ic = (n, cls = "") => `<svg class="gi ${cls}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${P[n]}</svg>`;

// ------------------------------------------------------------ HTML de GitHub, assaini
const KEEP = new Set("A ABBR B BLOCKQUOTE BR CODE DD DEL DETAILS DIV DL DT EM G-EMOJI H1 H2 H3 H4 H5 H6 HR I IMG INPUT INS KBD LI OL P PICTURE PRE S SAMP SOURCE SPAN STRONG SUB SUMMARY SUP TABLE TBODY TD TFOOT TH THEAD TR TT UL".split(" "));
const DROP = new Set("SCRIPT STYLE IFRAME OBJECT EMBED FORM BUTTON TEXTAREA SELECT TEMPLATE SVG MATH LINK META BASE CLIPBOARD-COPY".split(" "));
const ATTR = new Set(["href", "src", "srcset", "media", "alt", "title", "width", "height", "align", "open", "colspan", "rowspan", "type", "checked", "disabled"]);
const safeUrl = (u, img) => /^https:\/\//i.test(u) || (!img && (/^http:\/\//i.test(u) || /^mailto:/i.test(u) || u.startsWith("#")));
export function cleanHtml(html) {
  const doc = new DOMParser().parseFromString(`<div>${html || ""}</div>`, "text/html");
  const root = doc.body.firstChild;
  const walk = (el) => {
    for (const n of [...el.childNodes]) {
      if (n.nodeType === 8) { n.remove(); continue; }
      if (n.nodeType !== 1) continue;
      if (DROP.has(n.tagName)) { n.remove(); continue; }
      walk(n);
      if (!KEEP.has(n.tagName)) { n.replaceWith(...n.childNodes); continue; }
      for (const a of [...n.attributes]) {
        const v = a.value.trim();
        const ok = ATTR.has(a.name) && (a.name === "href" ? safeUrl(v, false) : a.name === "src" ? safeUrl(v, true) : a.name === "srcset" ? v.split(",").every((x) => safeUrl(x.trim(), true)) : true);
        if (!ok) n.removeAttribute(a.name);
      }
      if (n.tagName === "INPUT" && n.getAttribute("type") !== "checkbox") { n.remove(); continue; }
      if (n.tagName === "INPUT") n.setAttribute("disabled", "");
      if (n.tagName === "A") { n.setAttribute("target", "_blank"); n.setAttribute("rel", "noopener noreferrer"); }
      if (n.tagName === "IMG") n.setAttribute("loading", "lazy");
    }
  };
  walk(root);
  return root.innerHTML;
}

// Texte d'un commentaire pour un prompt : sans commentaires HTML ni images de décor.
const plain = (body) => String(body || "")
  .replace(/<!--[\s\S]*?-->/g, "")
  .replace(/<picture>[\s\S]*?<\/picture>/gi, (m) => (/alt="([^"]*)"/.exec(m) || [, ""])[1] + " ")
  .replace(/<img[^>]*alt="([^"]*)"[^>]*>/gi, "$1 ")
  .replace(/<\/?(details|summary|sub|sup|strong|b|em|br|p|div|span)[^>]*>/gi, "")
  .replace(/\n{3,}/g, "\n\n").trim();

const av = (who, size = 32) => who?.avatar
  ? `<img class="ghp-av" src="${esc(who.avatar)}" width="${size}" height="${size}" alt="">`
  : `<span class="ghp-av ghp-av-x" style="width:${size}px;height:${size}px">${esc((who?.login || "?")[0].toUpperCase())}</span>`;
const bot = (login) => (/\[bot\]$|^copilot/i.test(login || "") ? `<span class="ghp-label">bot</span>` : "");
const when = (at) => (at ? `<span class="ghp-muted" title="${esc(new Date(at).toLocaleString("fr-FR"))}">${esc(ago(Date.parse(at)))}</span>` : "");
const isCopilot = (l) => /^copilot(-pull-request-reviewer)?(\[bot\])?$/i.test(l || "");
const shortLogin = (l) => (isCopilot(l) ? "Copilot" : l);
const ghLogin = (l) => (isCopilot(l) ? "@copilot" : l);

function hunk(h) {
  if (!h) return "";
  const lines = h.split("\n").slice(-6);
  return `<pre class="ghp-hunk">${lines.map((l) => `<span class="${l.startsWith("@@") ? "h" : l.startsWith("+") ? "a" : l.startsWith("-") ? "d" : ""}">${esc(l) || " "}</span>`).join("")}</pre>`;
}

const STATE = { open: ["pr", "Ouverte"], draft: ["draft", "Brouillon"], merged: ["merged", "Fusionnée"], closed: ["prclosed", "Fermée"] };
const REVIEW = { approved: ["check", "ok", "a approuvé ces changements"], changes_requested: ["changes", "ko", "a demandé des changements"], commented: ["eye", "", "a relu"], dismissed: ["x", "", "revue écartée"] };
const failed = (c) => c.status === "done" && !["success", "skipped", "neutral", "cancelled"].includes(c.conclusion);
const ciIcon = (c) => (c.status === "pending" ? `<span class="ghp-ci pending"></span>`
  : c.conclusion === "success" ? ic("check", "ok") : ["skipped", "neutral", "cancelled"].includes(c.conclusion) ? `<span class="ghp-ci skip">–</span>` : ic("x", "ko"));

/** target : `{chat}` ou `{session}` ; toClaude(text) : confie un prompt à la conversation. */
export async function openGithub(target, title, { toClaude } = {}) {
  document.querySelector(".review")?.remove();
  const q = new URLSearchParams(target).toString();
  const panel = document.createElement("aside");
  panel.className = "review gh-panel";
  panel.innerHTML = `<div class="ghp-top">${icon("github")}<span class="ghp-top-t">${esc(title || "GitHub")}</span><span style="flex:1"></span>
    <button class="ghp-iconbtn" data-refresh title="Actualiser">${icon("sync")}</button><button class="ghp-iconbtn" data-close title="Fermer">${icon("close")}</button></div>
    <div class="ghp-scroll">${loader()}</div>`;
  document.body.append(panel);
  const body = $(".ghp-scroll", panel);
  let timer = null, candidates = null, s = null;
  const opened = new Set(); // repliables ouverts, gardés d'une actualisation à l'autre
  $("[data-close]", panel).addEventListener("click", () => { clearTimeout(timer); panel.remove(); });
  $("[data-refresh]", panel).addEventListener("click", () => draw());

  const act = async (btn, json, okMsg) => {
    if (btn) btn.disabled = true;
    try {
      const r = await api("/api/gh/action", { method: "POST", json: { ...target, ...json } });
      if (okMsg) toast(typeof okMsg === "function" ? okMsg(r) : okMsg);
      await draw(true);
      return r;
    } catch (e) { toast(e.message, true); if (btn) btn.disabled = false; }
  };
  const ask = (text, what) => {
    if (!toClaude) return toast("Cette conversation ne peut pas recevoir de prompt.", true);
    toClaude(text);
    toast(`${what} confié à Claude.`);
  };

  // ---------------------------------------------------------- morceaux de page
  const localBar = () => {
    const onBase = s.branch === s.base;
    const facts = [
      s.dirty && `${s.dirty} fichier${s.dirty > 1 ? "s" : ""} modifié${s.dirty > 1 ? "s" : ""}`,
      !s.upstream && !onBase && "jamais poussée",
      s.unpushed && `${s.unpushed} commit${s.unpushed > 1 ? "s" : ""} à pousser`,
      s.unpulled && `${s.unpulled} à récupérer`,
      !onBase && s.behind && `<span class="ghp-warn">${s.behind} de retard sur ${esc(s.base)}</span>`,
    ].filter(Boolean);
    return `<div class="ghp-local">
      <div class="ghp-local-h">${icon("branch")}<span>Copie locale</span><code class="ghp-ref">${esc(s.branch)}</code><span class="ghp-muted">${facts.length ? facts.join(" · ") : "à jour"}</span></div>
      <div class="ghp-row">
        <button class="ghp-btn sm" data-commit-open${s.dirty ? "" : " disabled"}>Committer…</button>
        <button class="ghp-btn sm" data-push${s.unpushed || (!s.upstream && !onBase) ? "" : " disabled"}>Pousser</button>
        <button class="ghp-btn sm" data-pull${s.unpulled ? "" : " disabled"}>Récupérer</button>
        ${onBase ? "" : `<button class="ghp-btn sm" data-merge-local${s.ahead && !s.dirty ? "" : " disabled"} title="Fusionner dans ${esc(s.base)} sans passer par GitHub">Fusionner en local</button>`}
        ${!onBase && s.behind ? `<button class="ghp-btn sm" data-update>${ic("claude")}Se mettre à jour (Claude)</button>` : ""}
      </div>
      <div class="ghp-commit" hidden>
        <textarea class="ghp-input" rows="3" placeholder="Message de commit"></textarea>
        <div class="ghp-row"><button class="ghp-btn sm" data-gen>${ic("claude")}Proposer</button><span style="flex:1"></span>
          <button class="ghp-btn sm" data-commit>Committer</button><button class="ghp-btn sm primary" data-commit-push>Committer et pousser</button></div>
      </div>
    </div>`;
  };

  const reviewersBox = (pr) => `<div class="ghp-side">
    <div class="ghp-side-h"><span>Relecteurs</span><button class="ghp-iconbtn" data-reviewers title="Demander une relecture">${ic("gear")}</button></div>
    ${pr.reviewers.length ? pr.reviewers.map((r) => `<div class="ghp-rev">${av(r.who, 20)}<b>${esc(shortLogin(r.who.login))}</b><span style="flex:1"></span>${
      r.state === "requested" ? `<span title="Relecture demandée">${ic("dot", "wait")}</span>`
        : `<button class="ghp-iconbtn sm" data-rerequest="${esc(ghLogin(r.who.login))}" title="Redemander une relecture">${icon("sync")}</button>${REVIEW[r.state] ? `<span title="${REVIEW[r.state][2]}">${ic(REVIEW[r.state][0], REVIEW[r.state][1])}</span>` : ""}`}</div>`).join("")
      : `<div class="ghp-muted small">Personne pour l'instant.</div>`}
    <div class="ghp-pop" hidden></div>
  </div>`;

  const commentBox = (c, key, verb = "a commenté") => `<div class="ghp-item">
    ${av(c.who)}
    <div class="ghp-box">
      <div class="ghp-box-h"><b>${esc(shortLogin(c.who.login))}</b>${bot(c.who.login)}<span class="ghp-muted">${verb}</span>${when(c.at)}<span style="flex:1"></span>
        ${c.body?.trim() ? `<button class="ghp-iconbtn" data-ask="${key}" title="Confier à Claude">${ic("claude")}</button>` : ""}</div>
      <div class="ghp-md">${cleanHtml(c.html) || `<p class="ghp-muted">Aucune description.</p>`}</div>
    </div></div>`;

  const threadBox = (t, i) => `<div class="ghp-thread${t.resolved ? " resolved" : ""}">
    <div class="ghp-thread-h">${ic("file")}<a href="${esc(t.comments[0]?.url || "#")}" target="_blank" rel="noopener">${esc(t.path)}${t.line ? `:${t.line}` : ""}</a>
      ${t.outdated ? `<span class="ghp-label warn">Obsolète</span>` : ""}${t.resolved ? `<span class="ghp-label">Résolu${t.resolved_by ? ` par ${esc(shortLogin(t.resolved_by))}` : ""}</span>` : ""}<span style="flex:1"></span>
      ${t.resolved ? "" : `<button class="ghp-iconbtn" data-ask="t${i}" title="Confier ce fil à Claude">${ic("claude")}</button>`}</div>
    ${hunk(t.hunk)}
    ${t.comments.map((c) => `<div class="ghp-tc">${av(c.who, 24)}<div class="ghp-tc-b"><div class="ghp-tc-h"><b>${esc(shortLogin(c.who.login))}</b>${bot(c.who.login)}${when(c.at)}</div><div class="ghp-md">${cleanHtml(c.html)}</div></div></div>`).join("")}
  </div>`;

  const folded = (key, iconHtml, label, inner) => `<details class="ghp-fold" data-key="${key}"${opened.has(key) ? " open" : ""}>
    <summary>${iconHtml}<span>${label}</span><span style="flex:1"></span><span class="ghp-show"></span></summary><div class="ghp-fold-b">${inner}</div></details>`;

  const timeline = (pr) => {
    const out = [];
    const items = pr.thread;
    for (let i = 0; i < items.length; i++) {
      const t = items[i];
      if (t.kind === "thread" && t.resolved) {
        // Fils résolus consécutifs : une seule banderole, repliée par défaut.
        const group = [];
        let j = i;
        while (j < items.length && items[j].kind === "thread" && items[j].resolved) group.push(j++);
        out.push(folded(`res-${i}`, ic("check", "ok"), group.length > 1 ? `${group.length} conversations résolues` : `Conversation résolue · <code>${esc(t.path)}</code>`, group.map((k) => threadBox(items[k], k)).join("")));
        i = j - 1;
      } else if (t.kind === "comment") {
        out.push(t.hidden ? folded(`hid-${i}`, ic("eye"), `Commentaire masqué de ${esc(shortLogin(t.who.login))}${t.hidden_why ? ` (${esc(t.hidden_why)})` : ""}`, commentBox(t, i)) : commentBox(t, i));
      } else if (t.kind === "review") {
        const [icn, cls, verb] = REVIEW[t.state] || ["eye", "", "a relu"];
        out.push(`<div class="ghp-event"><span class="ghp-badge ${cls}">${ic(icn)}</span>${av(t.who, 20)}<b>${esc(shortLogin(t.who.login))}</b><span class="ghp-muted">${verb}</span>${when(t.at)}</div>`);
        if (t.body?.trim()) out.push(commentBox(t, i, "a laissé une revue"));
      } else if (t.kind === "thread") {
        out.push(`<div class="ghp-item">${av(t.comments[0]?.who)}${threadBox(t, i)}</div>`);
      }
    }
    return out.join("") || `<div class="ghp-muted small ghp-pad">Aucun commentaire.</div>`;
  };

  const mergeBox = (pr) => {
    const checks = pr.checks;
    const nOk = checks.filter((c) => c.status === "done" && c.conclusion === "success").length, nKo = checks.filter(failed).length, nRun = checks.filter((c) => c.status === "pending").length;
    const rev = pr.review_decision === "approved" ? ["check", "ok", "Changements approuvés", "La PR a reçu les relectures requises."]
      : pr.review_decision === "changes_requested" ? ["changes", "ko", "Changements demandés", "Un relecteur demande des changements."]
      : pr.review_decision === "review_required" ? ["dot", "wait", "Relecture requise", "Au moins une relecture approuvée est requise."]
      : ["eye", "", "Pas de relecture requise", "Rien n'empêche la fusion côté relecture."];
    const ci = !checks.length ? null : nKo ? ["x", "ko", "Certains checks ont échoué"] : nRun ? ["dot", "wait", "Checks en cours"] : ["check", "ok", "Tous les checks sont passés"];
    const conflict = pr.mergeable === "conflicting";
    const open = pr.state === "open";
    const level = pr.gate?.level || "ok";
    const tone = !open ? pr.state : level === "draft" ? "draft" : level === "rules" || level === "conflict" ? "ko" : level === "confirm" ? "wait" : "ok";
    return `<div class="ghp-merge">
      <span class="ghp-merge-ic ${tone}">${ic(pr.state === "merged" ? "merged" : pr.state === "closed" ? "prclosed" : "pr")}</span>
      <div class="ghp-mbox">
        ${!open ? `<div class="ghp-mrow"><div><b>${pr.state === "merged" ? "PR fusionnée" : "PR fermée"}</b></div></div>` : `
        <div class="ghp-mrow">${ic(rev[0], rev[1])}<div><b>${rev[2]}</b><div class="ghp-muted">${rev[3]}</div></div></div>
        ${ci ? `<details class="ghp-mrow-d" data-key="checks"${opened.has("checks") || nKo ? " open" : ""}><summary class="ghp-mrow">${ic(ci[0], ci[1])}<div><b>${ci[2]}</b><div class="ghp-muted">${[nKo && `${nKo} en échec`, nRun && `${nRun} en cours`, nOk && `${nOk} réussi${nOk > 1 ? "s" : ""}`].filter(Boolean).join(", ")}</div></div><span style="flex:1"></span><span class="ghp-show"></span></summary>
          <div class="ghp-checks">${checks.map((c, i) => `<div class="ghp-check">${ciIcon(c)}<span class="ghp-check-n">${c.workflow ? `<span class="ghp-muted">${esc(c.workflow)} / </span>` : ""}${esc(c.name)}</span>
            ${failed(c) && c.run_id ? `<button class="ghp-btn xs" data-rerun="${c.run_id}">Relancer</button><button class="ghp-btn xs" data-fixci="${i}">${ic("claude")}Corriger</button>` : ""}
            <a class="ghp-link" href="${esc(c.url || pr.url)}" target="_blank" rel="noopener">Détails</a></div>`).join("")}</div></details>`
          : `<div class="ghp-mrow">${ic("dot")}<div><b>Aucun check</b><div class="ghp-muted">Aucune CI ne tourne sur cette PR.</div></div></div>`}
        <div class="ghp-mrow">${conflict ? ic("alert", "ko") : ic("check", "ok")}<div><b>${conflict ? "Cette branche a des conflits" : "Aucun conflit avec la branche de base"}</b><div class="ghp-muted">${conflict ? "Ils doivent être résolus avant la fusion." : "La fusion peut se faire automatiquement."}</div></div>
          ${conflict ? `<span style="flex:1"></span><button class="ghp-btn sm" data-conflicts>${ic("claude")}Résoudre (Claude)</button>` : ""}</div>
        ${mergeFoot(pr)}`}
      </div></div>`;
  };

  // Pied de la boîte de fusion, d'après le verdict du serveur (`pr.gate`) : bloqué, à confirmer, ou libre.
  const mergeFoot = (pr) => {
    const g = pr.gate || { level: "ok", reasons: [] };
    if (g.level === "draft") return `<div class="ghp-mfoot"><button class="ghp-btn" data-ready>Marquer prête pour relecture</button><span class="ghp-muted small">La PR est un brouillon.</span></div>`;
    const why = g.reasons.length ? `<ul class="ghp-why">${g.reasons.map((r) => `<li>${esc(r)}</li>`).join("")}</ul>` : "";
    const head = g.level === "conflict" ? `<div class="ghp-gate ko">${ic("alert", "ko")}<div><b>Fusion impossible</b>${why}</div></div>`
      : g.level === "rules" ? `<div class="ghp-gate ko">${ic("alert", "ko")}<div><b>Fusion bloquée par les règles du dépôt</b>${why}${pr.can_admin ? "" : `<div class="ghp-muted small">Tu n'as pas les droits pour contourner ces règles.</div>`}</div></div>`
      : g.level === "confirm" ? `<div class="ghp-gate warn">${ic("alert", "wait")}<div><b>La CI n'est pas verte</b>${why}</div></div>` : "";
    const can = g.level !== "conflict";
    return `<div class="ghp-mfoot col">${head}
      ${can ? `<div class="ghp-mopts">
        ${g.level === "confirm" ? `<label class="ghp-check-l"><input type="checkbox" data-force> Je confirme fusionner malgré la CI</label>` : ""}
        ${g.level === "rules" && pr.can_admin ? `<label class="ghp-check-l danger"><input type="checkbox" data-admin> Contourner les règles et fusionner maintenant (administrateur)</label>` : ""}
        <label class="ghp-check-l"><input type="checkbox" data-auto> Fusion automatique dès que les conditions sont remplies</label>
      </div>` : ""}
      <div class="ghp-row"><div class="ghp-split"><button class="ghp-btn primary" data-merge disabled>Squash and merge</button><label class="ghp-btn primary ghp-caret" title="Méthode de fusion">${icon("chevron")}<select data-method><option value="squash">Squash and merge</option><option value="merge">Create a merge commit</option><option value="rebase">Rebase and merge</option></select></label></div>
        ${g.level === "conflict" ? `<span class="ghp-muted small">Résous les conflits pour pouvoir fusionner.</span>` : ""}</div>
    </div>`;
  };

  // ---------------------------------------------------------- dessin
  async function draw(quiet = false) {
    clearTimeout(timer);
    const keep = { msg: $(".ghp-commit textarea", body)?.value || "", open: $(".ghp-commit", body)?.hidden === false, comment: $("[data-comment]", body)?.value || "", top: body.scrollTop };
    $$("details[data-key]", body).forEach((d) => (d.open ? opened.add(d.dataset.key) : opened.delete(d.dataset.key)));
    if (!quiet) body.innerHTML = loader();
    try { s = await api(`/api/gh?${q}`); } catch (e) { body.innerHTML = `<div class="ghp-pad ghp-err">${esc(e.message)}</div>`; return; }
    const pr = s.pr, onBase = s.branch === s.base;
    const unresolved = (pr?.thread || []).map((t, i) => [t, i]).filter(([t]) => t.kind === "thread" && !t.resolved);
    const stKey = pr ? (pr.state === "open" && pr.draft ? "draft" : pr.state) : "";
    const [stIcon, stLabel] = STATE[stKey] || STATE.open;
    body.innerHTML = `
      ${pr ? `<header class="ghp-head">
        <h2><a href="${esc(pr.url)}" target="_blank" rel="noopener">${esc(pr.title)}</a> <span class="ghp-num">#${pr.number}</span></h2>
        <div class="ghp-sub"><span class="ghp-state ${stKey}">${ic(stIcon)}${stLabel}</span>
          <span><b>${esc(pr.author?.login || "")}</b> veut fusionner ${pr.commits} commit${pr.commits > 1 ? "s" : ""} dans <code class="ghp-ref">${esc(pr.base)}</code> depuis <code class="ghp-ref">${esc(pr.head || s.branch)}</code></span></div>
      </header>` : `<header class="ghp-head"><h2>${esc(s.branch)}</h2><div class="ghp-sub ghp-muted">${onBase ? `Cette conversation travaille directement sur ${esc(s.base)}.` : "Pas de pull request pour cette branche."}</div></header>`}
      ${localBar()}
      ${!s.has_gh ? `<div class="ghp-pad ghp-muted">gh n'est pas installé : la PR, la CI et les relectures ne sont pas disponibles.</div>`
        : s.gh_error ? `<div class="ghp-pad ghp-err">${esc(s.gh_error)}</div>`
        : onBase ? "" : !pr ? `
      <div class="ghp-merge"><span class="ghp-merge-ic">${ic("pr")}</span><div class="ghp-mbox">
        <div class="ghp-mrow"><div><b>${s.ahead ? `${s.ahead} commit${s.ahead > 1 ? "s" : ""} d'avance sur ${esc(s.base)}` : "Rien à proposer"}</b><div class="ghp-muted">${s.ahead ? "La branche sera poussée, puis la PR créée avec le titre et la description des commits." : `La branche n'a pas de commit de plus que ${esc(s.base)}.`}</div></div></div>
        <div class="ghp-mfoot"><button class="ghp-btn primary" data-pr${s.ahead ? "" : " disabled"}>Créer la pull request</button><button class="ghp-btn" data-pr-draft${s.ahead ? "" : " disabled"}>Créer en brouillon</button></div>
      </div></div>` : `
      ${reviewersBox(pr)}
      ${unresolved.length ? `<div class="ghp-todo">${ic("comment")}<span>${unresolved.length} conversation${unresolved.length > 1 ? "s" : ""} non résolue${unresolved.length > 1 ? "s" : ""}</span><span style="flex:1"></span><button class="ghp-btn sm" data-all-threads>${ic("claude")}Tout confier à Claude</button></div>` : ""}
      <div class="ghp-timeline">${timeline(pr)}</div>
      ${mergeBox(pr)}
      <div class="ghp-item ghp-new">${av(pr.viewer)}<div class="ghp-box">
        <div class="ghp-box-h"><span class="ghp-tab">Écrire</span></div>
        <div class="ghp-new-b"><textarea class="ghp-input" rows="3" data-comment placeholder="Ajouter un commentaire"></textarea>
          <div class="ghp-row"><span class="ghp-muted small">Markdown accepté</span><span style="flex:1"></span><button class="ghp-btn primary sm" data-post>Commenter</button></div></div>
      </div></div>`}`;
    body.scrollTop = keep.top;
    wire(pr, keep, unresolved);
    if (pr?.checks.some((c) => c.status === "pending") && document.body.contains(panel)) timer = setTimeout(() => draw(true), 15000);
  }

  // ---------------------------------------------------------- actions
  function wire(pr, keep, unresolved) {
    const commitBox = $(".ghp-commit", body), msg = $("textarea", commitBox);
    msg.value = keep.msg;
    commitBox.hidden = !keep.open || !s.dirty;
    $("[data-commit-open]", body).addEventListener("click", () => { commitBox.hidden = !commitBox.hidden; msg.focus(); });
    $("[data-gen]", body).addEventListener("click", async (e) => {
      const b = e.currentTarget; b.disabled = true;
      try { msg.value = (await api("/api/review/message", { method: "POST", json: target })).message; } catch (err) { toast(err.message, true); }
      b.disabled = false;
    });
    const commit = async (btn, push) => {
      if (!msg.value.trim()) return msg.focus();
      const r = await act(btn, { action: "commit", message: msg.value }, push ? null : (r) => `Commité (${r.hash}).`);
      if (r && push) await act(null, { action: "push" }, "Commité et poussé.");
    };
    $("[data-commit]", body).addEventListener("click", (e) => commit(e.currentTarget, false));
    $("[data-commit-push]", body).addEventListener("click", (e) => commit(e.currentTarget, true));
    $("[data-push]", body).addEventListener("click", (e) => act(e.currentTarget, { action: "push" }, "Poussé."));
    $("[data-pull]", body).addEventListener("click", (e) => act(e.currentTarget, { action: "pull" }, "Récupéré."));
    $("[data-merge-local]", body)?.addEventListener("click", (e) => {
      if (confirm(`Fusionner ${s.branch} dans ${s.base}, dans le dépôt principal ?`)) act(e.currentTarget, { action: "merge_local" }, (r) => `${r.merged} fusionnée dans ${r.into}.`);
    });
    $("[data-update]", body)?.addEventListener("click", () => ask(
      `Mets la branche \`${s.branch}\` à jour avec \`origin/${s.base}\` : \`git fetch\`, puis fusionne \`origin/${s.base}\` dans la branche (pas de rebase, pas de push forcé). Résous les conflits éventuels en gardant l'intention des deux côtés, vérifie que ça compile et que les tests touchés passent, puis committe et pousse.`,
      "La mise à jour"));
    $("[data-pr]", body)?.addEventListener("click", (e) => { if (confirm(`Pousser ${s.branch} et créer une PR vers ${s.base} ?`)) act(e.currentTarget, { action: "pr" }, (r) => { if (r.url) window.open(r.url, "_blank"); return "PR créée."; }); });
    $("[data-pr-draft]", body)?.addEventListener("click", (e) => act(e.currentTarget, { action: "pr", draft: true }, "PR brouillon créée."));
    if (!pr) return;
    const comment = $("[data-comment]", body);
    comment.value = keep.comment;

    // Relecteurs : candidats (Copilot, collaborateurs), chargés au premier clic sur la roue.
    const pop = $(".ghp-pop", body);
    $("[data-reviewers]", body).addEventListener("click", async () => {
      if (!pop.hidden) { pop.hidden = true; return; }
      pop.hidden = false;
      pop.innerHTML = loader(true);
      candidates ||= await api(`/api/gh/reviewers?${q}`).catch(() => ["@copilot"]);
      const state = new Map(pr.reviewers.map((r) => [ghLogin(r.who.login).toLowerCase(), r.state]));
      const list = candidates.filter((l) => state.get(l.toLowerCase()) !== "requested");
      pop.innerHTML = `<div class="ghp-pop-h">Demander une relecture</div>` + (list.length ? list.map((l) => `<button class="ghp-pop-i" data-login="${esc(l)}">${l === "@copilot"
        ? `<span class="ghp-av ghp-av-x" style="width:20px;height:20px">${ic("claude")}</span><b>Copilot</b>`
        : `<img class="ghp-av" src="https://github.com/${encodeURIComponent(l)}.png?size=40" width="20" height="20" alt=""><b>${esc(l)}</b>`}${state.has(l.toLowerCase()) ? `<span class="ghp-muted small" style="margin-left:auto">Redemander</span>` : ""}</button>`).join("")
        : `<div class="ghp-muted small ghp-pad">Tous les relecteurs possibles sont déjà en attente.</div>`);
      $$("[data-login]", pop).forEach((b) => b.addEventListener("click", () => act(b, { action: "review", login: b.dataset.login }, `Relecture demandée à ${b.dataset.login === "@copilot" ? "Copilot" : b.dataset.login}.`)));
    });

    $$("[data-rerequest]", body).forEach((b) => b.addEventListener("click", () =>
      act(b, { action: "review", login: b.dataset.rerequest }, `Relecture redemandée à ${shortLogin(b.dataset.rerequest.replace(/^@/, ""))}.`)));

    const checks = pr.checks;
    $("[data-conflicts]", body)?.addEventListener("click", () => ask(
      `La PR #${pr.number} a des conflits avec \`${pr.base}\`. Fais \`git fetch\`, fusionne \`origin/${pr.base}\` dans la branche (pas de rebase, pas de push forcé), résous chaque conflit en gardant l'intention des deux côtés, vérifie que ça compile et que les tests touchés passent, puis committe et pousse.`,
      "La résolution des conflits"));
    $$("[data-rerun]", body).forEach((b) => b.addEventListener("click", (e) => { e.preventDefault(); act(b, { action: "rerun", run_id: +b.dataset.rerun }, "CI relancée."); }));
    $$("[data-fixci]", body).forEach((b) => b.addEventListener("click", async (e) => {
      e.preventDefault();
      const c = checks[+b.dataset.fixci];
      b.disabled = true;
      const r = await api(`/api/gh/log?${q}&run=${c.run_id}`).catch((err) => { toast(err.message, true); return null; });
      b.disabled = false;
      if (r) ask(`La CI de la PR #${pr.number} échoue sur le check « ${c.name} »${c.workflow ? ` (workflow ${c.workflow})` : ""}. Fin du journal des étapes en échec :\n\n\`\`\`\n${r.log}\n\`\`\`\n\nTrouve la cause, corrige-la, vérifie en local, puis committe et pousse.`, "La correction de la CI");
    }));
    $("[data-ready]", body)?.addEventListener("click", (e) => act(e.currentTarget, { action: "ready" }, "PR prête pour relecture."));
    // Bouton de fusion : actif seulement quand le verdict le permet (ou une case l'autorise).
    const method = $("[data-method]", body), mergeBtn = $("[data-merge]", body);
    const level = pr.gate?.level || "ok";
    const box = (sel) => $(sel, body)?.checked || false;
    const sync = () => {
      if (!mergeBtn) return;
      const auto = box("[data-auto]"), admin = box("[data-admin]"), force = box("[data-force]");
      // Fusion automatique et contournement s'excluent : cocher l'un décoche l'autre.
      mergeBtn.disabled = level === "conflict" || !(level === "ok" || auto || (level === "confirm" && force) || (level === "rules" && admin));
      mergeBtn.classList.toggle("danger", admin);
      $(".ghp-caret", body)?.classList.toggle("danger", admin);
      mergeBtn.textContent = auto ? "Activer la fusion automatique" : admin ? "Contourner et fusionner" : method.selectedOptions[0].textContent;
    };
    $("[data-auto]", body)?.addEventListener("change", (e) => { if (e.target.checked && $("[data-admin]", body)) $("[data-admin]", body).checked = false; sync(); });
    $("[data-admin]", body)?.addEventListener("change", (e) => { if (e.target.checked) $("[data-auto]", body).checked = false; sync(); });
    $("[data-force]", body)?.addEventListener("change", sync);
    method?.addEventListener("change", sync);
    sync();
    mergeBtn?.addEventListener("click", (e) => {
      const auto = box("[data-auto]"), admin = box("[data-admin]"), force = box("[data-force]"), label = method.selectedOptions[0].textContent;
      const what = auto ? `Activer la fusion automatique (${label}) de la PR #${pr.number} ?`
        : admin ? `Contourner les règles du dépôt et fusionner la PR #${pr.number} vers ${pr.base} maintenant ?`
        : `${label} : PR #${pr.number} vers ${pr.base} ?`;
      if (!confirm(what)) return;
      act(e.currentTarget, { action: "merge", method: method.value, auto, admin, force }, (r) => (r.auto ? "Fusion automatique activée." : r.admin ? "PR fusionnée (règles contournées)." : "PR fusionnée."));
    });

    // Prompts confiés à Claude : un commentaire, un fil, ou tous les fils non résolus (jamais les résolus).
    const threadText = (t) => `\`${t.path}${t.line ? ":" + t.line : ""}\`${t.outdated ? " (obsolète)" : ""}\n` + t.comments.map((c) => `  ${shortLogin(c.who.login)} : ${plain(c.body).replace(/\n/g, "\n  ")}`).join("\n");
    $$("[data-ask]", body).forEach((b) => b.addEventListener("click", () => {
      const k = b.dataset.ask;
      if (k.startsWith("t")) {
        return ask(`Fil de review non résolu sur la PR #${pr.number}, ${threadText(pr.thread[+k.slice(1)])}\n\nTraite-le : corrige si c'est fondé, sinon explique-moi pourquoi. Puis committe et pousse.`, "Le fil");
      }
      const t = pr.thread[+k];
      ask(`Commentaire de ${shortLogin(t.who.login)} sur la PR #${pr.number} :\n\n${plain(t.body)}\n\nTraite-le : corrige ce qui est fondé, sinon explique-moi pourquoi. Puis committe et pousse.`, "Le commentaire");
    }));
    $("[data-all-threads]", body)?.addEventListener("click", () => ask(
      `Traite les ${unresolved.length} fils de review non résolus de la PR #${pr.number} (les fils résolus sont exclus) : pour chacun, corrige si c'est fondé, sinon dis-moi pourquoi. Puis committe et pousse.\n\n${unresolved.map(([t]) => "- " + threadText(t)).join("\n")}`,
      "Les fils non résolus"));
    $("[data-post]", body).addEventListener("click", async (e) => {
      if (!comment.value.trim()) return comment.focus();
      const r = await act(e.currentTarget, { action: "comment", body: comment.value }, "Commentaire publié.");
      if (r) $("[data-comment]", body).value = "";
    });
  }
  draw();
}
