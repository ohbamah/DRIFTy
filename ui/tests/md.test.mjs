// Rendu Markdown : il se termine sur TOUT début de texte (le texte en cours d'écriture est
// rendu coupé n'importe où). Une ligne `|` seule bouclait sans fin et figeait l'app.
import { test } from "node:test";
import assert from "node:assert/strict";
import { md } from "../js/md.js";

test("une ligne de tableau inachevée est un paragraphe", () => {
  assert.equal(md("|"), "<p>|</p>");
  assert.equal(md("| a | b"), "<p>| a | b</p>");
  assert.equal(md("texte\n\n| a"), "<p>texte</p><p>| a</p>");
});

test("chaque début d'une réponse avec tableau, liste, titre et code se termine", () => {
  const reply = "## Titre\n\nUn `code` et **gras** :\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- un\n- deux\n\n```js\nx = 1\n```\n#### fin |\n| x";
  for (let i = 0; i <= reply.length; i++) assert.equal(typeof md(reply.slice(0, i)), "string");
  assert.match(md(reply), /<table><tr><th>a<\/th><th>b<\/th><\/tr><tr><td>1<\/td><td>2<\/td><\/tr><\/table>/);
});

test("un bloc de code porte ses boutons Copier et Transférer", () => {
  const h = md("Message :\n```\nagent, fais X\n```\nSuite");
  assert.match(h, /^<p>Message :<\/p><div class="code-block"><div class="code-actions">.*data-code-copy.*data-code-transfer.*<\/div><pre><code>agent, fais X<\/code><\/pre><\/div><p>Suite<\/p>$/);
});
