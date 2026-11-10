// Panneau « Commandes » : commande de test et de lancement du projet (proposées
// par l'IA ou écrites à la main), exécutées dans le dossier de la conversation.
import { api, esc, icon, toast, loader, $ } from "./lib.js";
import { ansiToHtml } from "./ansi.js";

/** Panneau de sortie d'une exécution lancée par DRIFTy (mise en place d'un worktree…). */
export function openJob(jobId, title) {
  document.querySelector(".review")?.remove();
  const panel = document.createElement("aside");
  panel.className = "review";
  panel.innerHTML = `<div class="rv-head"><b>${esc(title || "Sortie")}</b><span style="flex:1"></span><button class="icon-btn" data-close title="Fermer">${icon("close")}</button></div>
    <div class="rv-body"><div class="console-head"><span class="muted small" data-status></span></div><pre class="console"></pre></div>`;
  document.body.append(panel);
  const out = $(".console", panel), status = $("[data-status]", panel);
  let next = 0, raw = "", timer;
  panel.querySelector("[data-close]").addEventListener("click", () => { clearTimeout(timer); panel.remove(); });
  const poll = async () => {
    const r = await api(`/api/run/${jobId}?from=${next}`).catch((e) => { status.textContent = e.message; return null; });
    if (!r) return;
    next = r.next;
    if (r.output) { raw += r.output; out.innerHTML = ansiToHtml(raw); out.scrollTop = out.scrollHeight; }
    status.innerHTML = r.done
      ? (r.code === 0 ? `<span style="color:var(--gain)">✓ Terminé</span>` : `<span style="color:var(--error)">✗ Échec (code ${r.code ?? "?"})</span>`)
      : `<span class="spin" style="width:12px;height:12px;border-width:2px;display:inline-block;vertical-align:-1px"></span> En cours : <code>${esc(r.command)}</code>`;
    if (!r.done && panel.isConnected) timer = setTimeout(poll, 700);
  };
  poll();
}

export async function openRun(target, repoId, title, { onResult } = {}) {
  document.querySelector(".review")?.remove();
  const panel = document.createElement("aside");
  panel.className = "review";
  panel.innerHTML = `<div class="rv-head"><b>Commandes</b><span class="muted small">${esc(title || "")}</span><span style="flex:1"></span><button class="icon-btn" data-close title="Fermer">${icon("close")}</button></div><div class="rv-body">${loader()}</div>`;
  document.body.append(panel);
  let job = null, timer = null, raw = "";
  panel.querySelector("[data-close]").addEventListener("click", () => { clearInterval(timer); panel.remove(); });
  const body = panel.querySelector(".rv-body");
  if (!repoId) { body.innerHTML = `<div class="err small">Le projet de cette conversation est inconnu : lance une analyse.</div>`; return; }
  const cmds = await api(`/api/repos/${repoId}/commands`).catch(() => ({ test: "", run: "" }));
  body.innerHTML = `
    <div class="cmd-row"><label>Tests</label><input class="field mono" id="c-test" value="${esc(cmds.test)}" placeholder="ex. make unit-fast"><button class="btn primary" data-run="test">▶ Lancer</button></div>
    <div class="cmd-row"><label>Lancement</label><input class="field mono" id="c-run" value="${esc(cmds.run)}" placeholder="ex. foreman start"><button class="btn" data-run="run">▶ Lancer</button></div>
    <div class="rv-actions" style="margin:6px 0 14px"><button class="btn" data-analyze>${icon("sparkle")}Analyser les lancements</button><span class="muted small" id="c-why"></span></div>
    <div class="console-head"><span id="c-status" class="muted small">Aucune exécution.</span><span style="flex:1"></span>
      <button class="btn" data-stop disabled>Arrêter</button><button class="btn" data-send disabled>Envoyer à Claude</button></div>
    <div class="run-info" id="c-info" hidden></div>
    <pre class="console" id="c-out"></pre>`;
  const out = $("#c-out", body), status = $("#c-status", body), info = $("#c-info", body);
  // Adresses locales annoncées par le serveur (Puma, Vite, Next…), et port déjà pris.
  const URL_RE = /https?:\/\/(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1?\]|[\w-]+\.local|[\w-]+\.localhost)(?::\d+)?[^\s"'<>)\]]*/g;
  const drawInfo = () => {
    const plain = raw.replace(/\x1b\[[0-9;?]*[A-Za-z]/g, "");
    const urls = [...new Set((plain.match(URL_RE) || []).map((u) => u.replace("0.0.0.0", "localhost").replace(/[.,;:]+$/, "")))];
    const busy = /address already in use|EADDRINUSE|port \d+ is (?:already )?in use/i.test(plain);
    const parts = [];
    if (urls.length) parts.push(`<span class="run-urls">🌐 ${urls.slice(-3).map((u) => `<a href="${esc(u)}" target="_blank" rel="noopener">${esc(u)}</a>`).join(" ")}</span>`);
    if (job?.port) parts.push(`<span class="muted">Port du worktree : <code>${job.port}</code> (variable PORT)</span>`);
    if (job?.hardcoded) parts.push(`<span class="run-warn">⚠ Port fixé en dur, PORT ignoré — ${esc(job.hardcoded)} ; écris <code>-p \${PORT:-3000}</code></span>`);
    if (busy) parts.push(`<span class="run-warn">⚠ Port déjà utilisé par un autre serveur</span>`);
    info.innerHTML = parts.join("");
    info.hidden = !parts.length;
  };
  const save = () => api(`/api/repos/${repoId}/commands`, { method: "PUT", json: { test: $("#c-test", body).value, run: $("#c-run", body).value } }).catch(() => {});
  body.querySelectorAll("input").forEach((i) => i.addEventListener("change", save));
  body.querySelector("[data-analyze]").addEventListener("click", async (e) => {
    e.target.disabled = true;
    e.target.innerHTML = `<span class="spin" style="width:14px;height:14px;border-width:2px"></span> Analyse…`;
    try {
      const r = await api(`/api/repos/${repoId}/commands/analyze`, { method: "POST" });
      $("#c-test", body).value = r.test; $("#c-run", body).value = r.run; $("#c-why", body).textContent = r.why;
      toast("Commandes proposées et enregistrées.");
    } catch (err) { toast(err.message, true); }
    e.target.disabled = false; e.target.innerHTML = `${icon("sparkle")}Analyser les lancements`;
  });
  const poll = async () => {
    const r = await api(`/api/run/${job.id}?from=${job.next}`).catch(() => null);
    if (!r) return;
    job.next = r.next;
    if (r.output) {
      // Sortie brute gardée (codes ANSI compris), affichée en couleurs ; seule la fin est rendue.
      raw += r.output;
      if (raw.length > 300_000) raw = raw.slice(-250_000);
      const stick = out.scrollHeight - out.scrollTop - out.clientHeight < 40;
      out.innerHTML = ansiToHtml(raw);
      drawInfo();
      if (stick) out.scrollTop = out.scrollHeight;
    }
    if (r.done) {
      clearInterval(timer);
      // Une commande de lancement n'est pas censée finir : son arrêt n'est jamais un « succès ».
      status.innerHTML = job.kind === "run"
        ? `<span style="color:var(--waste)">⚠ La commande s'est arrêtée (code ${r.code ?? "?"})${job.stopped ? ", à ta demande" : ""}</span>`
        : r.code === 0 ? `<span style="color:var(--gain)">✓ Tests terminés (code 0)</span>` : `<span style="color:var(--error)">✗ Échec (code ${r.code ?? "?"})</span>`;
      body.querySelector("[data-stop]").disabled = true;
      body.querySelector("[data-send]").disabled = false;
      job.code = r.code;
    }
  };
  body.querySelectorAll("[data-run]").forEach((b) => b.addEventListener("click", async () => {
    const command = $(b.dataset.run === "test" ? "#c-test" : "#c-run", body).value.trim();
    if (!command) return toast("Commande vide : écris-la ou lance l'analyse.", true);
    save();
    clearInterval(timer);
    out.textContent = "";
    raw = "";
    try {
      const r = await api("/api/run", { method: "POST", json: { ...target, command } });
      job = { id: r.id, next: 0, command, kind: b.dataset.run, port: r.port, hardcoded: r.hardcoded };
      drawInfo();
      status.innerHTML = `<span class="spin" style="width:12px;height:12px;border-width:2px;display:inline-block;vertical-align:-1px"></span> ${b.dataset.run === "run" ? "En marche" : "En cours"} : <code>${esc(command)}</code>`;
      body.querySelector("[data-stop]").disabled = false;
      body.querySelector("[data-send]").disabled = true;
      timer = setInterval(poll, 700);
    } catch (e) { toast(e.message, true); }
  }));
  body.querySelector("[data-stop]").addEventListener("click", () => { if (!job) return; job.stopped = true; api(`/api/run/${job.id}`, { method: "DELETE" }).catch(() => {}); });
  body.querySelector("[data-send]").addEventListener("click", () => {
    const tail = out.textContent.split("\n").slice(-60).join("\n"); // texte sans codes de couleur
    const text = `Résultat de \`${job.command}\` (code ${job.code ?? "?"}) — dernières lignes :\n\n\`\`\`\n${tail}\n\`\`\``;
    onResult?.(text);
  });
}
