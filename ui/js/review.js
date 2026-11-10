// Panneau « Changements » d'une conversation : état git du worktree, diff coloré,
// commit (message écrit ou proposé par l'IA), fusion dans la base, PR.
import { api, esc, icon, toast, loader, diffStat, $ } from "./lib.js";

/** Découpe un diff unifié par fichier, avec ses lignes ajoutées / retirées. */
function files(diff) {
  if (!diff) return [];
  return diff.split(/^(?=diff --git )/m).filter((x) => x.startsWith("diff --git")).map((chunk) => {
    const name = (/^\+\+\+ b\/(.+)$/m.exec(chunk) || /^diff --git a\/(.+?) b\//m.exec(chunk) || [, "?"])[1];
    const lines = chunk.split("\n");
    const add = lines.filter((l) => l.startsWith("+") && !l.startsWith("+++")).length;
    const del = lines.filter((l) => l.startsWith("-") && !l.startsWith("---")).length;
    const body = lines.slice(lines.findIndex((l) => l.startsWith("@@"))).map((l) => {
      const cls = l.startsWith("@@") ? "h" : l.startsWith("+") ? "a" : l.startsWith("-") ? "d" : "";
      return `<span class="${cls}">${esc(l) || " "}</span>`;
    }).join("\n");
    return { name, add, del, body };
  });
}

const fileList = (fs) => fs.map((f) => `<details class="rv-file"><summary>${icon("chevron")}<span class="rv-name">${esc(f.name)}</span>${diffStat([f.add, f.del])}</summary><pre class="rv-diff">${f.body}</pre></details>`).join("");

export async function openReview(target, title) {
  document.querySelector(".review")?.remove();
  const q = new URLSearchParams(target).toString();
  const panel = document.createElement("aside");
  panel.className = "review";
  panel.innerHTML = `<div class="rv-head"><b>Changements</b><span class="muted small">${esc(title || "")}</span><span style="flex:1"></span><button class="icon-btn" data-close title="Fermer">${icon("close")}</button></div><div class="rv-body">${loader()}</div>`;
  document.body.append(panel);
  panel.querySelector("[data-close]").addEventListener("click", () => panel.remove());
  const body = panel.querySelector(".rv-body");
  const draw = async () => {
    body.innerHTML = loader();
    let s;
    try { s = await api(`/api/review?${q}`); } catch (e) { body.innerHTML = `<div class="err small" style="padding:16px">${esc(e.message)}</div>`; return; }
    const work = files(s.work_diff), branch = files(s.branch_diff);
    const dirty = s.status.length > 0;
    body.innerHTML = `
      <div class="rv-branch">${icon("branch")}<code>${esc(s.branch)}</code><span class="muted">→ ${esc(s.base)}</span>
        ${s.ahead ? `<span class="chip">${s.ahead} commit${s.ahead > 1 ? "s" : ""} d'avance</span>` : ""}${s.behind ? `<span class="chip">${s.behind} de retard</span>` : ""}</div>
      <h3>Non commité ${dirty ? `<span class="muted small">(${s.status.length} fichier${s.status.length > 1 ? "s" : ""})</span>` : ""}</h3>
      ${dirty ? fileList(work) + (s.work_cut ? `<div class="muted small">Diff tronqué.</div>` : "") : `<div class="muted small">Rien à committer.</div>`}
      ${dirty ? `<div class="rv-commit">
        <textarea class="field area" id="rv-msg" rows="3" placeholder="Message de commit"></textarea>
        <div class="rv-actions"><button class="btn" data-gen>${icon("sparkle")}Proposer un message</button><span style="flex:1"></span><button class="btn primary" data-commit>Committer</button></div>
      </div>` : ""}
      ${s.commits.length ? `<h3>Commits de la branche</h3><ul class="rv-commits">${s.commits.map((c) => `<li><code>${esc(c.hash)}</code> ${esc(c.subject)}</li>`).join("")}</ul>${fileList(branch)}${s.branch_cut ? `<div class="muted small">Diff tronqué.</div>` : ""}` : ""}
      ${s.branch !== s.base ? `<div class="rv-actions rv-final">
        <button class="btn" data-merge${s.ahead ? "" : " disabled"}>Fusionner dans ${esc(s.base)}</button>
        <button class="btn" data-pr${s.ahead && s.has_gh ? "" : " disabled"} title="${s.has_gh ? "" : "gh n'est pas installé"}">Créer une PR</button>
      </div>` : `<div class="muted small" style="margin-top:14px">Cette conversation travaille directement sur ${esc(s.base)}.</div>`}`;
    const act = (sel, path, okMsg, confirmMsg, extra = () => ({})) => body.querySelector(sel)?.addEventListener("click", async (e) => {
      if (confirmMsg && !confirm(confirmMsg)) return;
      e.target.disabled = true;
      try { const r = await api(path, { method: "POST", json: { ...target, ...extra() } }); toast(okMsg(r)); draw(); }
      catch (err) { toast(err.message, true); e.target.disabled = false; }
    });
    act("[data-commit]", "/api/review/commit", (r) => `Commité (${r.hash}).`, null, () => ({ message: $("#rv-msg", body).value }));
    act("[data-merge]", "/api/review/merge", (r) => `${r.merged} fusionnée dans ${r.into}.`, `Fusionner ${s.branch} dans ${s.base} (dans le dépôt principal) ?`);
    act("[data-pr]", "/api/review/pr", (r) => { if (r.url) window.open(r.url, "_blank"); return "PR créée."; }, `Pousser ${s.branch} et créer une PR vers ${s.base} ?`);
    body.querySelector("[data-gen]")?.addEventListener("click", async (e) => {
      e.target.disabled = true;
      try { const r = await api("/api/review/message", { method: "POST", json: target }); $("#rv-msg", body).value = r.message; }
      catch (err) { toast(err.message, true); }
      e.target.disabled = false;
    });
  };
  draw();
}
