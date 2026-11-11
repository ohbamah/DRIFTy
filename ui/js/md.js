// Rendu Markdown de l'interface, par markdown-it (ui/vendor/markdown-it, v14.1.0, MIT) :
// listes numérotées et imbriquées, liens, citations, tableaux, code. Le HTML brut est
// DÉSACTIVÉ (tout est échappé) ; les liens s'ouvrent dans le navigateur du système.
// L'ancien rendu maison bouclait sans fin sur une ligne `|` et figeait l'app.
import MarkdownIt from "../vendor/markdown-it/markdown-it.js";
import { esc } from "./text.js";
import { icon } from "./icons.js";

// Bloc de code : boutons Copier (partout, voir main.js) et Transférer (dans une réponse, voir reply.js).
const codeActions = `<div class="code-actions"><button type="button" data-code-copy title="Copier le code" aria-label="Copier le code">${icon("copy")}</button><button type="button" data-code-transfer title="Transférer le code vers une autre conversation" aria-label="Transférer le code">${icon("forward")}</button></div>`;

const mdi = new MarkdownIt({ html: false, linkify: true, breaks: true });
const codeBlock = (code) => `<div class="code-block">${codeActions}<pre><code>${esc(code.replace(/\n$/, ""))}</code></pre></div>`;
mdi.renderer.rules.fence = (tokens, i) => codeBlock(tokens[i].content);
mdi.renderer.rules.code_block = (tokens, i) => codeBlock(tokens[i].content);
// Titres ramenés aux deux tailles du fil (# et ## → h3, ### et plus → h4).
const level = (tag) => (+tag.slice(1) <= 2 ? "h3" : "h4");
mdi.renderer.rules.heading_open = (tokens, i) => `<${level(tokens[i].tag)}>`;
mdi.renderer.rules.heading_close = (tokens, i) => `</${level(tokens[i].tag)}>\n`;
// Liens : nouvel onglet, que l'app confie au navigateur du système.
const linkOpen = mdi.renderer.rules.link_open || ((tokens, i, opts, env, self) => self.renderToken(tokens, i, opts));
mdi.renderer.rules.link_open = (tokens, i, opts, env, self) => {
  tokens[i].attrSet("target", "_blank");
  tokens[i].attrSet("rel", "noopener noreferrer");
  return linkOpen(tokens, i, opts, env, self);
};

export function md(src) {
  return mdi.render(String(src ?? ""));
}

/**
 * Texte en cours d'écriture : longueur de sa partie déjà COMPLÈTE (blocs fermés par une ligne
 * vide, hors bloc de code ouvert). Elle se rend une fois ; seule la suite est refaite à chaque
 * fragment, au lieu de toute la réponse.
 */
export function stableLength(text) {
  let fence = false, stable = 0, pos = 0;
  for (const line of text.split("\n")) {
    const end = pos + line.length + 1; // position après le saut de ligne
    if (/^\s{0,3}(```|~~~)/.test(line)) fence = !fence;
    else if (!fence && !line.trim() && end <= text.length) stable = end;
    pos = end;
  }
  return stable;
}
