// Histogramme SVG (simple ou empilé), avec infobulle par colonne.
import { esc, tok, showTip, hideTip } from "./lib.js";

function niceStep(raw) {
  const p = Math.pow(10, Math.floor(Math.log10(raw || 1)));
  const n = raw / p;
  return (n <= 1 ? 1 : n <= 2 ? 2 : n <= 5 ? 5 : 10) * p;
}

/**
 * rows : [{label, ...valeurs}] ; series : [{key, label, color}].
 * fmt : format des valeurs (axe + infobulle).
 */
export function bars(rows, series, { height = 200, fmt = tok, width = 720 } = {}) {
  // La largeur du SVG suit celle du conteneur : le texte garde sa taille réelle.
  const W = Math.max(280, Math.round(width || 720)), H = height, L = 48, R = 4, T = 8, B = 24;
  const iw = W - L - R, ih = H - T - B;
  const total = (r) => series.reduce((s, k) => s + (r[k.key] || 0), 0);
  const max = Math.max(...rows.map(total), 0) || 1;
  const step = niceStep(max / 4);
  const top = Math.ceil(max / step) * step;
  const y = (v) => T + ih - (v / top) * ih;
  const bw = iw / Math.max(rows.length, 1);
  const gap = Math.min(3, bw * 0.25);
  let svg = `<svg viewBox="0 0 ${W} ${H}" role="img">`;
  for (let v = 0; v <= top + 1e-9; v += step) {
    svg += `<line x1="${L}" x2="${W - R}" y1="${y(v)}" y2="${y(v)}" stroke="var(--line${v ? "-soft" : ""})"/>`;
    svg += `<text x="${L - 8}" y="${y(v) + 4}" text-anchor="end">${esc(fmt(v))}</text>`;
  }
  const every = Math.ceil(rows.length / 8);
  rows.forEach((r, i) => {
    const x = L + i * bw + gap / 2, w = Math.max(1, bw - gap);
    let acc = 0;
    const present = series.filter((s) => (r[s.key] || 0) > 0);
    present.forEach((s, si) => {
      const v = r[s.key];
      const y0 = y(acc), y1 = y(acc + v);
      const h = Math.max(0, y0 - y1 - (si > 0 ? 2 : 0));
      const rad = 0; // angles droits
      const delay = ` style="animation-delay:${Math.min(i, 40) * 12}ms"`;
      svg += rad
        ? `<path class="bar"${delay} d="M${x},${y0 - (si > 0 ? 2 : 0)} V${y1 + rad} Q${x},${y1} ${x + rad},${y1} H${x + w - rad} Q${x + w},${y1} ${x + w},${y1 + rad} V${y0 - (si > 0 ? 2 : 0)} Z" fill="${s.color}"/>`
        : `<rect class="bar"${delay} x="${x}" y="${y0 - h - (si > 0 ? 2 : 0)}" width="${w}" height="${h}" fill="${s.color}"/>`;
      acc += v;
    });
    if (i % every === 0) svg += `<text x="${x + w / 2}" y="${H - 6}" text-anchor="middle">${esc(r.label)}</text>`;
    svg += `<rect class="hit" data-i="${i}" x="${L + i * bw}" y="${T}" width="${bw}" height="${ih}" fill="transparent"/>`;
  });
  svg += "</svg>";
  const el = document.createElement("div");
  el.className = "chart";
  el.innerHTML =
    (series.length > 1 ? `<div class="legend">${series.map((s) => `<span><i style="background:${s.color}"></i>${esc(s.label)}</span>`).join("")}</div>` : "") + svg;
  el.querySelectorAll(".hit").forEach((h) => {
    const r = rows[+h.dataset.i];
    h.addEventListener("mousemove", (e) =>
      showTip(e, `<b>${esc(r.title || r.label)}</b>` + series.map((s) => `<div class="row"><span><i style="background:${s.color}"></i>${esc(s.label)}</span><span>${esc(fmt(r[s.key] || 0))}</span></div>`).join(""))
    );
    h.addEventListener("mouseleave", hideTip);
  });
  return el;
}
