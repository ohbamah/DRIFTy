// Sortie de terminal → HTML : couleurs ANSI (16, 256 et 24 bits), gras, souligné.
// Les autres séquences (curseur, titres) sont retirées ; « \r » réécrit la ligne.
import { esc } from "./lib.js";

const BASE = ["#5c6370", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#abb2bf"];
const BRIGHT = ["#7f848e", "#ff7b86", "#b5e890", "#ffd68a", "#7cc0ff", "#de92ff", "#70d6e2", "#e6e8ec"];

function color256(n) {
  if (n < 8) return BASE[n];
  if (n < 16) return BRIGHT[n - 8];
  if (n >= 232) { const v = 8 + (n - 232) * 10; return `rgb(${v},${v},${v})`; }
  const i = n - 16, f = (x) => (x ? 55 + x * 40 : 0);
  return `rgb(${f(Math.floor(i / 36))},${f(Math.floor(i / 6) % 6)},${f(i % 6)})`;
}

export function ansiToHtml(text) {
  // Fins de ligne de terminal ; un « \r » seul revient en début de ligne (barres de progression).
  const lines = text.replace(/\r\n/g, "\n").split("\n").map((l) => (l.includes("\r") ? l.slice(l.lastIndexOf("\r") + 1) : l));
  const src = lines.join("\n").replace(/\x1b\][^\x07\x1b]*(\x07|\x1b\\)/g, ""); // OSC (titre de fenêtre…)
  let out = "", st = { fg: null, bold: false, dim: false, ul: false, it: false }, open = false;
  const style = () => [st.fg && `color:${st.fg}`, st.bold && "font-weight:700", st.dim && "opacity:.65", st.ul && "text-decoration:underline", st.it && "font-style:italic"].filter(Boolean).join(";");
  const re = /\x1b\[([0-9;?]*)([A-Za-z])/g;
  let last = 0, m;
  const flush = (chunk) => { if (chunk) out += esc(chunk); };
  while ((m = re.exec(src))) {
    flush(src.slice(last, m.index));
    last = re.lastIndex;
    if (m[2] !== "m") continue; // déplacements de curseur, effacements : ignorés
    const codes = m[1] === "" ? [0] : m[1].split(";").map(Number);
    for (let i = 0; i < codes.length; i++) {
      const c = codes[i];
      if (c === 0) st = { fg: null, bold: false, dim: false, ul: false, it: false };
      else if (c === 1) st.bold = true;
      else if (c === 2) st.dim = true;
      else if (c === 3) st.it = true;
      else if (c === 4) st.ul = true;
      else if (c === 22) st.bold = st.dim = false;
      else if (c === 23) st.it = false;
      else if (c === 24) st.ul = false;
      else if (c >= 30 && c <= 37) st.fg = BASE[c - 30];
      else if (c >= 90 && c <= 97) st.fg = BRIGHT[c - 90];
      else if (c === 39) st.fg = null;
      else if (c === 38 && codes[i + 1] === 5) { st.fg = color256(codes[i + 2]); i += 2; }
      else if (c === 38 && codes[i + 1] === 2) { st.fg = `rgb(${codes[i + 2]},${codes[i + 3]},${codes[i + 4]})`; i += 4; }
      else if ((c === 48 || c === 58) && (codes[i + 1] === 5 || codes[i + 1] === 2)) i += codes[i + 1] === 5 ? 2 : 4; // fonds : ignorés
    }
    if (open) out += "</span>";
    const css = style();
    open = !!css;
    if (open) out += `<span style="${css}">`;
  }
  flush(src.slice(last).replace(/\x1b\[[0-9;?]*$/, "").replace(/\x1b/g, ""));
  if (open) out += "</span>";
  return out;
}
