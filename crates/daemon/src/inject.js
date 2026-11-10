// Injecté par le relais de DRIFTy dans les pages affichées dans l'onglet navigateur.
// Rôle : (1) garder la navigation dans le relais (liens absolus vers le vrai site) ;
// (2) dire à DRIFTy l'adresse réelle et le titre de la page ; (3) sélecteur d'éléments :
// survol en surbrillance, clic = l'élément (sélecteur, balise, texte, HTML, styles) est
// envoyé à DRIFTy pour en faire un prompt. Rien n'est actif hors d'un cadre de DRIFTy.
(() => {
  if (window.top === window || window.__drifty) return;
  window.__drifty = 1;
  const T = __TARGET__, R = location.origin;
  const real = (u) => {
    try { const x = new URL(u, location.href); return x.origin === R ? T + x.pathname + x.search + x.hash : x.href; } catch { return String(u); }
  };
  const send = (m) => parent.postMessage(Object.assign({ drifty: 1 }, m), "*");
  const nav = () => send({ type: "nav", url: real(location.href), title: document.title, relay: R });
  addEventListener("load", nav);
  addEventListener("popstate", nav);
  for (const k of ["pushState", "replaceState"]) {
    const orig = history[k];
    history[k] = function (...a) { const r = orig.apply(this, a); setTimeout(nav, 0); return r; };
  }
  nav();

  // Où envoyer une adresse : le relais de ce site, ou celui d'une autre origine (via /__drifty/go).
  const inside = (u) => {
    try {
      const x = new URL(u, location.href);
      if (x.origin === R || !/^https?:$/.test(x.protocol)) return null;
      return x.origin === T ? R + x.pathname + x.search + x.hash : R + "/__drifty/go?u=" + encodeURIComponent(x.href);
    } catch { return null; }
  };
  // Liens : réécrits au clic ; un nouvel onglet demandé devient un onglet de DRIFTy.
  document.addEventListener("click", (e) => {
    if (picking) return;
    const a = e.target.closest && e.target.closest("a[href]");
    if (!a || e.defaultPrevented) return;
    if (a.target === "_blank" || e.metaKey || e.ctrlKey) { e.preventDefault(); send({ type: "open", url: real(a.href) }); return; }
    const to = inside(a.href);
    if (to) a.href = to;
  }, true);
  window.open = function (u) { if (u) send({ type: "open", url: real(String(u)) }); return null; };
  // Formulaires vers le vrai site ou ailleurs (307 : méthode et corps gardés).
  document.addEventListener("submit", (e) => {
    const f = e.target;
    const to = f && f.action && inside(f.action);
    if (to) f.action = to;
  }, true);
  // Navigations faites par script (location.href = …) : interceptées là où le navigateur le permet.
  if (window.navigation) {
    navigation.addEventListener("navigate", (e) => {
      const to = inside(e.destination.url);
      if (to && e.cancelable && !e.destination.sameDocument) { e.preventDefault(); location.href = to; }
    });
  }

  // ---------------------------------------------------------------- sélecteur d'éléments
  let picking = false, current = null;
  const box = document.createElement("div"), label = document.createElement("div");
  box.style.cssText = "position:fixed;pointer-events:none;z-index:2147483647;border:2px solid #3b82f6;background:rgba(59,130,246,.12);border-radius:3px;display:none;transition:all .05s";
  label.style.cssText = "position:fixed;pointer-events:none;z-index:2147483647;background:#1d4ed8;color:#fff;font:11px/1.4 ui-monospace,Menlo,monospace;padding:2px 6px;border-radius:3px;display:none;white-space:nowrap";
  const mount = () => { if (!box.isConnected) document.documentElement.append(box, label); };

  const short = (el) => el.tagName.toLowerCase() + (el.id ? "#" + el.id : "") +
    [...el.classList].filter((c) => !/^(css|jsx|sc|svelte|tw)-|^[a-z]{1,3}[0-9]|_[a-z0-9]{5,}$/i.test(c)).slice(0, 2).map((c) => "." + c).join("");
  const selector = (el) => {
    const parts = [];
    for (let n = el; n && n.nodeType === 1 && parts.length < 7; n = n.parentElement) {
      if (n.id && !/\d{3,}/.test(n.id)) { parts.unshift("#" + CSS.escape(n.id)); break; }
      let p = short(n).replace(/#.*?(?=\.|$)/, "");
      const sib = n.parentElement ? [...n.parentElement.children].filter((c) => c.tagName === n.tagName) : [];
      if (sib.length > 1) p += `:nth-of-type(${sib.indexOf(n) + 1})`;
      parts.unshift(p);
      if (n === document.body) break;
    }
    return parts.join(" > ");
  };
  const KEYS = ["display", "position", "color", "background-color", "font-size", "font-weight", "font-family", "padding", "margin", "border", "border-radius", "width", "height", "gap"];
  const info = (el) => {
    const cs = getComputedStyle(el), r = el.getBoundingClientRect();
    const attrs = {};
    for (const a of el.attributes) if (a.name !== "class" && a.name !== "style" && a.value.length < 200) attrs[a.name] = a.value;
    return {
      selector: selector(el), tag: el.tagName.toLowerCase(), short: short(el), classes: [...el.classList], attrs,
      text: (el.innerText || el.textContent || "").trim().replace(/\s+/g, " ").slice(0, 300),
      html: el.outerHTML.length > 2500 ? el.outerHTML.slice(0, 2500) + "…" : el.outerHTML,
      styles: Object.fromEntries(KEYS.map((k) => [k, cs.getPropertyValue(k)]).filter(([, v]) => v && v !== "none" && v !== "normal" && v !== "0px" && v !== "auto")),
      size: `${Math.round(r.width)}×${Math.round(r.height)}`, url: real(location.href), title: document.title,
    };
  };
  const show = (el) => {
    current = el;
    const r = el.getBoundingClientRect();
    Object.assign(box.style, { display: "block", left: r.left + "px", top: r.top + "px", width: r.width + "px", height: r.height + "px" });
    label.textContent = `${short(el)}  ${Math.round(r.width)}×${Math.round(r.height)}`;
    Object.assign(label.style, { display: "block", left: Math.max(0, r.left) + "px", top: (r.top > 22 ? r.top - 22 : r.bottom + 4) + "px" });
  };
  const stop = (tell) => {
    picking = false; current = null;
    box.style.display = label.style.display = "none";
    document.documentElement.style.cursor = "";
    if (tell) send({ type: "pick-off" });
  };
  const block = (e) => { if (picking) { e.preventDefault(); e.stopPropagation(); } };
  addEventListener("mouseover", (e) => { if (picking && e.target !== box) show(e.target); }, true);
  addEventListener("mousedown", block, true);
  addEventListener("mouseup", block, true);
  addEventListener("click", (e) => {
    if (!picking) return;
    e.preventDefault(); e.stopPropagation();
    send({ type: "picked", info: info(current || e.target), add: e.shiftKey });
    if (!e.shiftKey) stop(true);
  }, true);
  addEventListener("keydown", (e) => { if (picking && e.key === "Escape") { e.preventDefault(); stop(true); } }, true);
  addEventListener("message", (e) => {
    if (e.source !== parent || !e.data || !e.data.drifty) return;
    const c = e.data.cmd;
    if (c === "pick") { if (e.data.on) { mount(); picking = true; document.documentElement.style.cursor = "crosshair"; } else stop(false); }
    if (c === "back") history.back();
    if (c === "forward") history.forward();
  });
})();
