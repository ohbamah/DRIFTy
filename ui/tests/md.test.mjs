// Rendu Markdown (markdown-it) : il se termine sur TOUT début de texte (le texte en cours
// d'écriture est rendu coupé n'importe où) ; l'ancien rendu bouclait sans fin sur `|`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { md, stableLength } from "../js/md.js";

const flat = (h) => h.replace(/\n/g, "");

test("une ligne de tableau inachevée est un paragraphe", () => {
  assert.equal(flat(md("|")), "<p>|</p>");
  assert.equal(flat(md("| a | b")), "<p>| a | b</p>");
});

test("chaque début d'une réponse avec tableau, liste, titre et code se termine", () => {
  const reply = "## Titre\n\nUn `code` et **gras** :\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n1. un\n2. deux\n   - sous\n\n```js\nx = 1\n```\n#### fin |\n| x";
  for (let i = 0; i <= reply.length; i++) assert.equal(typeof md(reply.slice(0, i)), "string");
  assert.match(flat(md(reply)), /<table><thead><tr><th>a<\/th><th>b<\/th><\/tr><\/thead><tbody><tr><td>1<\/td><td>2<\/td><\/tr><\/tbody><\/table>/);
});

test("listes numérotées et imbriquées, liens, citations, titres ramenés", () => {
  const h = flat(md("# Grand\n\n### Petit\n\n3. trois\n4. quatre\n   - sous\n\n> cité\n\nVoir https://example.com et [doc](https://example.com/d)."));
  assert.match(h, /^<h3>Grand<\/h3><h4>Petit<\/h4>/);
  assert.match(h, /<ol start="3"><li>trois<\/li><li>quatre<ul><li>sous<\/li><\/ul><\/li><\/ol>/);
  assert.match(h, /<blockquote><p>cité<\/p><\/blockquote>/);
  assert.match(h, /<a href="https:\/\/example.com" target="_blank" rel="noopener noreferrer">https:\/\/example.com<\/a>/);
  assert.match(h, /<a href="https:\/\/example.com\/d" target="_blank" rel="noopener noreferrer">doc<\/a>/);
});

test("le HTML brut est échappé, jamais interprété", () => {
  const h = md('<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>\n\n[x](javascript:alert(1))');
  assert.doesNotMatch(h, /<script|<img|href="javascript/);
  assert.match(h, /&lt;script&gt;/);
});

test("un bloc de code porte ses boutons Copier et Transférer", () => {
  const h = flat(md("Message :\n```\nagent, fais X\n\nsuite\n```\nSuite"));
  assert.match(h, /^<p>Message :<\/p><div class="code-block"><div class="code-actions">.*data-code-copy.*data-code-transfer.*<\/div><pre><code>agent, fais Xsuite<\/code><\/pre><\/div><p>Suite<\/p>$/);
});

test("le découpage du texte en cours ne coupe jamais un bloc de code ouvert", () => {
  assert.equal(stableLength("a\n\nb"), 3);
  assert.equal(stableLength("a\n\n```\nx\n\ny"), 3); // bloc ouvert : la ligne vide du code ne compte pas
  assert.equal(stableLength("a\n\n```\nx\n\ny\n```\n\nz"), 17);
  assert.equal(stableLength("rien de complet"), 0);
});

test("rendre par morceaux complets donne le même résultat que d'un coup", () => {
  const reply = "Intro **x**.\n\n## T\n\n- a\n- b\n\n```\nc\n\nd\n```\n\nFin `e`.\n";
  let html = "", done = 0;
  for (let i = 1; i <= reply.length; i++) {
    const k = stableLength(reply.slice(0, i));
    if (k > done) { html += md(reply.slice(done, k)); done = k; }
  }
  assert.equal(flat(html + md(reply.slice(done))), flat(md(reply)));
});
