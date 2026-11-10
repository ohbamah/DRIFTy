// Menus déroulants maison, à la place du menu natif de macOS. Tout `<select>` de
// l'interface est remplacé automatiquement par un bouton et une liste ; le `<select>`
// reste caché et fait foi (valeur, événement `change`, formulaire), donc le code qui
// le lit ou l'écrit ne change pas. `data-native` garde le menu natif.
import { esc, icon } from "./lib.js";

let openMenuEl = null;
const closeMenu = () => { openMenuEl?.close(); };

/**
 * Liste sous (ou au-dessus de) `anchor` : items `[{value, label, title?, disabled?}]`,
 * `current` coché ; `onPick(value)` au choix. Clavier : ↑ ↓, Entrée, Échap, lettres.
 */
export function openMenu(anchor, items, current, onPick) {
  closeMenu();
  const host = anchor.closest("dialog") || document.body; // un dialogue modal est au-dessus de tout
  const m = document.createElement("div");
  m.className = "csel-menu";
  m.setAttribute("role", "listbox");
  m.innerHTML = items.map((it, i) => `<div class="csel-item${it.value === current ? " on" : ""}${it.disabled ? " off" : ""}" role="option" data-i="${i}"${it.title ? ` title="${esc(it.title)}"` : ""}>
      <span class="csel-check">${it.value === current ? icon("check") : ""}</span><span class="csel-label">${esc(it.label)}</span></div>`).join("");
  host.append(m);
  const r = anchor.getBoundingClientRect();
  m.style.minWidth = Math.max(r.width, 160) + "px";
  const h = m.offsetHeight, below = innerHeight - r.bottom - 8;
  m.style.left = Math.min(r.left, innerWidth - m.offsetWidth - 8) + "px";
  m.style.top = (below >= h || below >= r.top ? r.bottom + 4 : Math.max(8, r.top - h - 4)) + "px";
  let cur = Math.max(0, items.findIndex((it) => it.value === current));
  const rows = [...m.querySelectorAll(".csel-item")];
  const hover = (i) => { rows.forEach((x, k) => x.classList.toggle("hl", k === i)); rows[i]?.scrollIntoView({ block: "nearest" }); cur = i; };
  hover(cur);
  const step = (d) => { let i = cur; for (let n = 0; n < items.length; n++) { i = (i + d + items.length) % items.length; if (!items[i].disabled) return hover(i); } };
  let typed = "", typedAt = 0;
  const keys = (e) => {
    if (e.key === "ArrowDown") step(1);
    else if (e.key === "ArrowUp") step(-1);
    else if (e.key === "Enter" || e.key === " ") { if (!items[cur]?.disabled) pick(cur); }
    else if (e.key === "Escape" || e.key === "Tab") { close(); if (e.key === "Tab") return; }
    else if (e.key.length === 1) {
      typed = (Date.now() - typedAt > 700 ? "" : typed) + e.key.toLowerCase();
      typedAt = Date.now();
      const i = items.findIndex((it) => !it.disabled && it.label.toLowerCase().startsWith(typed));
      if (i >= 0) hover(i);
    } else return;
    e.preventDefault();
    e.stopPropagation();
  };
  const outside = (e) => { if (!m.contains(e.target) && !anchor.contains(e.target)) close(); };
  const close = () => {
    m.remove();
    removeEventListener("keydown", keys, true);
    removeEventListener("mousedown", outside, true);
    removeEventListener("resize", close);
    document.removeEventListener("scroll", onScroll, true);
    anchor.classList.remove("open");
    if (openMenuEl === api) openMenuEl = null;
  };
  const onScroll = (e) => { if (!m.contains(e.target)) close(); };
  // Le focus revient au bouton AVANT l'action : une action qui ouvre un champ (renommer) le garde.
  const pick = (i) => { close(); anchor.focus?.(); onPick(items[i].value); };
  m.addEventListener("mousemove", (e) => { const it = e.target.closest(".csel-item"); if (it && !it.classList.contains("off")) hover(+it.dataset.i); });
  m.addEventListener("click", (e) => { const it = e.target.closest(".csel-item"); if (it && !it.classList.contains("off")) pick(+it.dataset.i); });
  addEventListener("keydown", keys, true);
  addEventListener("mousedown", outside, true);
  addEventListener("resize", close);
  document.addEventListener("scroll", onScroll, true);
  anchor.classList.add("open");
  const api = { close };
  openMenuEl = api;
  return api;
}

// ------------------------------------------------------------ remplacement des <select>
const itemsOf = (sel) => [...sel.options].map((o) => ({ value: o.value, label: o.textContent.trim(), title: o.title, disabled: o.disabled }));

function enhance(sel) {
  if (sel.dataset.native !== undefined || sel._csel) return;
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "csel " + [...sel.classList].join(" ");
  if (sel.title) btn.title = sel.title;
  btn.innerHTML = `<span class="csel-v"></span>${icon("chevron")}`;
  sel._csel = btn;
  sel.hidden = true;
  sel.after(btn);
  const draw = () => {
    const o = sel.options[sel.selectedIndex];
    btn.querySelector(".csel-v").textContent = o ? o.textContent.trim() : "";
    btn.disabled = sel.disabled;
  };
  // Une valeur écrite par le code (`sel.value = …`) met aussi le bouton à jour.
  const proto = HTMLSelectElement.prototype;
  for (const prop of ["value", "selectedIndex"]) {
    const d = Object.getOwnPropertyDescriptor(proto, prop);
    Object.defineProperty(sel, prop, { configurable: true, get() { return d.get.call(this); }, set(v) { d.set.call(this, v); draw(); } });
  }
  new MutationObserver(draw).observe(sel, { childList: true, subtree: true, attributes: true, attributeFilter: ["disabled", "selected"] });
  sel.addEventListener("change", draw);
  btn.addEventListener("click", () => {
    if (btn.classList.contains("open")) return closeMenu();
    openMenu(btn, itemsOf(sel), sel.value, (v) => {
      if (v === sel.value) return;
      sel.value = v;
      sel.dispatchEvent(new Event("change", { bubbles: true }));
      sel.dispatchEvent(new Event("input", { bubbles: true }));
    });
  });
  btn.addEventListener("keydown", (e) => { if (e.key === "ArrowDown" || e.key === "ArrowUp") { e.preventDefault(); btn.click(); } });
  draw();
}

/** Remplace les `<select>` présents puis ceux qui apparaîtront. */
export function autoSelects(root = document.body) {
  root.querySelectorAll("select").forEach(enhance);
  new MutationObserver((muts) => {
    for (const m of muts) for (const n of m.addedNodes) {
      if (n.nodeType !== 1) continue;
      if (n.tagName === "SELECT") enhance(n);
      else n.querySelectorAll?.("select").forEach(enhance);
    }
  }).observe(root, { childList: true, subtree: true });
}
