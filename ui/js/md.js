// Rendu Markdown minimal et sûr (tout est échappé avant mise en forme).
import { esc } from "./text.js";
import { icon } from "./icons.js";

// Bloc de code : boutons Copier (partout, voir main.js) et Transférer (dans une réponse, voir reply.js).
const codeActions = `<div class="code-actions"><button type="button" data-code-copy title="Copier le code" aria-label="Copier le code">${icon("copy")}</button><button type="button" data-code-transfer title="Transférer le code vers une autre conversation" aria-label="Transférer le code">${icon("forward")}</button></div>`;

function inline(s) {
  return s
    .replace(/`([^`]+)`/g, "<code>$1</code>")
    .replace(/\*\*([^*]+)\*\*/g, "<b>$1</b>")
    .replace(/(^|[^*])\*([^*\s][^*]*)\*/g, "$1<i>$2</i>");
}

export function md(src) {
  const out = [];
  const lines = String(src).split("\n");
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (line.startsWith("```")) {
      const buf = [];
      i++;
      while (i < lines.length && !lines[i].startsWith("```")) buf.push(lines[i++]);
      i++;
      out.push(`<div class="code-block">${codeActions}<pre><code>${esc(buf.join("\n"))}</code></pre></div>`);
      continue;
    }
    if (/^\s*\|.*\|\s*$/.test(line)) {
      const rows = [];
      while (i < lines.length && /^\s*\|.*\|\s*$/.test(lines[i])) rows.push(lines[i++]);
      const cells = (r) => r.trim().slice(1, -1).split("|").map((c) => inline(esc(c.trim())));
      const body = rows.filter((r) => !/^\s*\|[\s:|-]+\|\s*$/.test(r));
      out.push(`<table>${body.map((r, k) => `<tr>${cells(r).map((c) => (k === 0 ? `<th>${c}</th>` : `<td>${c}</td>`)).join("")}</tr>`).join("")}</table>`);
      continue;
    }
    if (/^\s*[-*] /.test(line)) {
      const items = [];
      while (i < lines.length && /^\s*[-*] /.test(lines[i])) items.push(lines[i++].replace(/^\s*[-*] /, ""));
      out.push(`<ul>${items.map((t) => `<li>${inline(esc(t))}</li>`).join("")}</ul>`);
      continue;
    }
    const h = line.match(/^(#{1,4}) (.*)/);
    if (h) {
      out.push(`<h${h[1].length > 2 ? 4 : 3}>${inline(esc(h[2]))}</h${h[1].length > 2 ? 4 : 3}>`);
      i++;
      continue;
    }
    if (!line.trim()) {
      i++;
      continue;
    }
    // La 1re ligne est toujours prise : une ligne qu'aucun bloc ne reconnaît (`| a` sans `|`
    // final, une ligne de tableau à moitié écrite) bouclait ici sans fin et figeait la page.
    const para = [lines[i++]];
    while (i < lines.length && lines[i].trim() && !/^(```|#{1,4} |\s*[-*] |\s*\|)/.test(lines[i])) para.push(lines[i++]);
    out.push(`<p>${inline(esc(para.join("\n"))).replace(/\n/g, "<br>")}</p>`);
  }
  return out.join("");
}
