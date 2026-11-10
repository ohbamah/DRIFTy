// Indicateur glissant pour les sélecteurs segmentés (.seg) et les onglets (.tabs).
export function slide(root = document) {
  for (const group of root.querySelectorAll(".seg, .tabs")) {
    let ind = group.querySelector(":scope > .ind");
    if (!ind) {
      ind = document.createElement("span");
      ind.className = "ind";
      group.prepend(ind);
    }
    const on = group.querySelector(".on");
    if (!on) continue;
    const place = (animate) => {
      if (!animate) ind.style.transition = "none";
      ind.style.width = on.offsetWidth + "px";
      ind.style.transform = `translateX(${on.offsetLeft}px)`;
      if (!animate) requestAnimationFrame(() => (ind.style.transition = ""));
    };
    place(group.classList.contains("ready"));
    group.classList.add("ready");
  }
}

/** Déplace l'indicateur vers `btn` avant que la vue ne se redessine. */
export function moveTo(btn) {
  const group = btn.closest(".seg, .tabs");
  const ind = group?.querySelector(":scope > .ind");
  if (!ind) return;
  group.querySelectorAll(".on").forEach((b) => b.classList.remove("on"));
  btn.classList.add("on");
  ind.style.width = btn.offsetWidth + "px";
  ind.style.transform = `translateX(${btn.offsetLeft}px)`;
}

/** Fait monter un pourcentage de 0 à sa valeur. */
export function countUp(el, to, ms = 900) {
  const t0 = performance.now();
  const step = (t) => {
    const k = Math.min(1, (t - t0) / ms);
    el.textContent = Math.round(to * (1 - Math.pow(1 - k, 3))) + " %";
    if (k < 1) requestAnimationFrame(step);
  };
  if (matchMedia("(prefers-reduced-motion: reduce)").matches) el.textContent = Math.round(to) + " %";
  else requestAnimationFrame(step);
}
