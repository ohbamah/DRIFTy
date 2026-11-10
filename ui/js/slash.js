// Commandes `/` dans la saisie : dès que le premier caractère est `/`, la liste des
// commandes de la conversation (celles qu'annonce Claude Code, skills du projet compris,
// sauf /clear et /btw) s'affiche et se filtre ; ↑ ↓ pour choisir, Entrée ou Tab pour
// compléter, Échap pour fermer.
import { api, esc } from "./lib.js";

const cache = new Map(); // cible → promesse de la liste

/** target() : `{chat}` ou `{session}` ; à appeler AVANT que la saisie n'écoute Entrée. */
export function attachSlash(ta, target, container = ta.parentElement) {
  const box = document.createElement("div");
  box.className = "slash-menu";
  box.hidden = true;
  container.append(box);
  let items = [], sel = 0, open = false;

  const load = () => {
    const t = target();
    const k = new URLSearchParams(t).toString();
    if (!cache.has(k)) cache.set(k, api(`/api/commands?${k}`).catch(() => { cache.delete(k); return []; }));
    return cache.get(k);
  };
  const close = () => { open = false; box.hidden = true; };
  const draw = () => {
    box.innerHTML = items.length
      ? items.map((c, i) => `<div class="slash-item${i === sel ? " on" : ""}" data-i="${i}"><span class="slash-name">/${esc(c.name)}</span>${c.hint ? `<span class="slash-hint">${esc(c.hint)}</span>` : ""}<span class="slash-desc">${esc((c.description || "").replace(/\s*\((user|project|plugin|bundled|dynamic workflow)[^)]*\)\s*$/, ""))}</span></div>`).join("")
      : `<div class="slash-empty">Aucune commande ne correspond.</div>`;
    box.querySelector(".slash-item.on")?.scrollIntoView({ block: "nearest" });
  };
  const update = async () => {
    const v = ta.value;
    // Seulement tant que l'on écrit le NOM de la commande (premier mot, commencé par /).
    const m = /^\/([^\s]*)$/.exec(v.slice(0, ta.selectionStart ?? v.length));
    if (!m || v.indexOf(" ") !== -1 && v.indexOf(" ") < (ta.selectionStart ?? 0)) return close();
    const q = m[1].toLowerCase();
    const all = await load();
    if (ta.value !== v) return; // la saisie a changé pendant le chargement
    const starts = all.filter((c) => c.name.toLowerCase().startsWith(q));
    const inside = all.filter((c) => !c.name.toLowerCase().startsWith(q) && c.name.toLowerCase().includes(q));
    items = [...starts, ...inside].slice(0, 60);
    sel = Math.min(sel, Math.max(0, items.length - 1));
    open = true;
    box.hidden = false;
    draw();
  };
  const pick = (c) => {
    const rest = ta.value.replace(/^\/\S*/, "");
    ta.value = `/${c.name}${rest.startsWith(" ") ? rest : " " + rest.trimStart()}`;
    const at = c.name.length + 2;
    ta.setSelectionRange(at, at);
    close();
    ta.dispatchEvent(new Event("input", { bubbles: true }));
    ta.focus();
  };
  ta.addEventListener("input", () => { sel = 0; update(); });
  ta.addEventListener("focus", () => { load(); }, { once: true }); // liste préparée avant le premier « / »
  ta.addEventListener("blur", () => setTimeout(close, 150));
  ta.addEventListener("keydown", (e) => {
    if (!open || !items.length) { if (open && e.key === "Escape") { close(); e.stopImmediatePropagation(); } return; }
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      sel = (sel + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
      draw();
    } else if ((e.key === "Enter" && !e.shiftKey && !e.isComposing) || e.key === "Tab") {
      pick(items[sel]);
    } else if (e.key === "Escape") {
      close();
    } else return;
    e.preventDefault();
    e.stopImmediatePropagation();
  });
  box.addEventListener("mousedown", (e) => {
    const it = e.target.closest(".slash-item");
    if (!it) return;
    e.preventDefault();
    pick(items[+it.dataset.i]);
  });
}
