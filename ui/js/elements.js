// Éléments de page joints à un message (choisis dans l'onglet navigateur), comme des
// images : la saisie montre `[Élément #1]` et une pastille ; à l'envoi, chaque élément
// part en bloc `<drifty-element>` à la fin du message ; le fil le montre replié.
import { esc } from "./lib.js";

export const token = (n) => `[Élément #${n}]`;

/** Bloc envoyé à Claude pour un élément (page, sélecteur, balise, texte, styles, HTML). */
export function elementBlock(n, e) {
  const lines = [
    `Élément de la page ${e.url}${e.title ? ` (« ${e.title} »)` : ""}, choisi dans le navigateur de DRIFTy : retrouve dans le code ce qui le produit avant de modifier.`,
    `Sélecteur : \`${e.selector}\``,
    `Balise : \`<${e.tag}${e.classes.length ? ` class="${e.classes.join(" ")}"` : ""}>\` (${e.size} px)`,
    Object.keys(e.attrs).length ? `Attributs : ${Object.entries(e.attrs).map(([k, v]) => `${k}="${v}"`).join(", ")}` : "",
    e.text ? `Texte : « ${e.text} »` : "",
    Object.keys(e.styles).length ? `Styles calculés : ${Object.entries(e.styles).map(([k, v]) => `${k}: ${v}`).join("; ")}` : "",
    "```html\n" + e.html + "\n```",
  ].filter(Boolean);
  return `<drifty-element n="${n}" label="${e.short.replace(/"/g, "'")}">\n${lines.join("\n")}\n</drifty-element>`;
}

/** Texte final : le message, puis le bloc de chaque élément joint (dans l'ordre des numéros). */
export function withElements(text, elements) {
  if (!elements.length) return text;
  return `${text}\n\n${elements.map((e) => elementBlock(e.n, e.info)).join("\n\n")}`;
}

/** Affichage d'un message de l'utilisateur : blocs d'éléments repliés en pastilles. */
export function userText(text) {
  const re = /<drifty-element n="(\d+)" label="([^"]*)">\n?([\s\S]*?)\n?<\/drifty-element>/g;
  let out = "", last = 0, m;
  const chips = [];
  while ((m = re.exec(text))) {
    out += esc(text.slice(last, m.index));
    chips.push(`<details class="el-chip"><summary><span class="el-tag">Élément #${m[1]}</span><code>${esc(m[2])}</code></summary><pre>${esc(m[3])}</pre></details>`);
    last = re.lastIndex;
  }
  out += esc(text.slice(last));
  return out.replace(/\s+$/, "") + (chips.length ? `<div class="el-chips">${chips.join("")}</div>` : "");
}
