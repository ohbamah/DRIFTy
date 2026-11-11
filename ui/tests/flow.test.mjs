// Modèle des automations : mêmes refus que le serveur (automations.rs).
import { test } from "node:test";
import assert from "node:assert/strict";
import { newFlow, addNode, connect, removeNode, removeEdge, linkRefusal, edgePath } from "../js/flow.js";

test("un flow part d'un Start fixe et se relie sans boucle", () => {
  const f = newFlow("Revue");
  assert.deepEqual(f.nodes.map((n) => n.type), ["start"]);
  const ai = addNode(f, "ai", 300, 200), sh = addNode(f, "bash", 500, 200), wh = addNode(f, "webhook", 700, 200);
  assert.equal(ai.config.model, "sonnet");
  assert.equal(connect(f, "start", ai.id), null);
  assert.equal(connect(f, ai.id, sh.id), null);
  assert.equal(connect(f, sh.id, wh.id), null);
  assert.match(connect(f, wh.id, ai.id), /boucle/);
  assert.match(connect(f, ai.id, "start"), /Start/);
  assert.match(connect(f, ai.id, ai.id), /lui-même/);
  assert.match(connect(f, ai.id, sh.id), /déjà/);
  assert.equal(f.edges.length, 3);
});

test("le Start ne se supprime pas ; un nœud retiré emporte ses liens", () => {
  const f = newFlow();
  const a = addNode(f, "github", 1, 1);
  connect(f, "start", a.id);
  assert.equal(removeNode(f, "start"), false);
  assert.equal(removeNode(f, a.id), true);
  assert.deepEqual(f.edges, []);
  assert.throws(() => addNode(f, "start", 0, 0));
  assert.throws(() => addNode(f, "slack", 0, 0));
});

test("retirer un lien rouvre le chemin", () => {
  const f = newFlow();
  const a = addNode(f, "ai", 0, 0), b = addNode(f, "bash", 0, 0);
  connect(f, a.id, b.id);
  assert.match(linkRefusal(f, b.id, a.id), /boucle/);
  removeEdge(f, a.id, b.id);
  assert.equal(linkRefusal(f, b.id, a.id), null);
  assert.equal(edgePath(0, 0, 100, 50), "M0,0 C50,0 50,50 100,50");
});
