// Fil d'une conversation : on arrive en bas, et un bouton ↓ (en bas à gauche, au-dessus
// de la saisie) y ramène quand on est remonté loin. Un seul mécanisme pour la discussion
// en direct et la conversation passée.
import { icon } from "./lib.js";

/**
 * Descend en bas, puis y reste pendant que la mise en page se stabilise (saisie ajoutée
 * après coup, images, blocs de code), tant que l'utilisateur n'a pas fait défiler.
 */
export function toBottom(scroller) {
  const down = () => { scroller.scrollTop = scroller.scrollHeight; };
  down();
  let moved = false;
  const stop = () => { moved = true; };
  scroller.addEventListener("wheel", stop, { once: true, passive: true });
  scroller.addEventListener("touchstart", stop, { once: true, passive: true });
  const ro = new ResizeObserver(() => { if (!moved) down(); });
  ro.observe(scroller);
  for (const c of scroller.children) ro.observe(c);
  requestAnimationFrame(() => { if (!moved) down(); });
  setTimeout(() => ro.disconnect(), 1500);
}

/** Bouton ↓ posé dans `host` (le conteneur du fil et de la saisie), au-dessus de la saisie. */
export function jumpButton(scroller, host) {
  host.querySelector(":scope > .jump-bottom")?.remove();
  const b = document.createElement("button");
  b.type = "button";
  b.className = "jump-bottom";
  b.title = "Revenir en bas";
  b.setAttribute("aria-label", "Revenir en bas");
  b.innerHTML = icon("chevron");
  b.hidden = true;
  host.append(b);
  const place = () => {
    if (!scroller.isConnected) { b.remove(); return; }
    const far = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight > 600;
    b.hidden = !far || !scroller.offsetParent;
    if (b.hidden) return;
    const h = host.getBoundingClientRect(), s = scroller.getBoundingClientRect();
    b.style.left = `${s.left - h.left + 16}px`;
    b.style.bottom = `${h.bottom - s.bottom + 14}px`;
  };
  scroller.addEventListener("scroll", place, { passive: true });
  new ResizeObserver(place).observe(scroller);
  b.addEventListener("click", () => scroller.scrollTo({ top: scroller.scrollHeight, behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" }));
  place();
}
