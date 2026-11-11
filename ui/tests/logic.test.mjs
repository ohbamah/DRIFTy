// Règles de l'interface sans DOM : `node --test ui/tests/*.test.mjs` (ou `./test.sh`).
import { test } from "node:test";
import assert from "node:assert/strict";
import { rowStatus } from "../js/status.js";
import { askModel } from "../js/ask.js";
import { bashMeta, bashRow, asksPassword, appendTail } from "../js/bashrow.js";
import { planChoices, keepPlanningMessage, planOutcome } from "../js/plan.js";

test("statut : attente > travail > surveillance > arrêt", () => {
  assert.equal(rowStatus({ chat: { pending: 1, waiting_for: "question", status: "working" } }), "ask");
  assert.equal(rowStatus({ chat: { pending: 1, waiting_for: "plan" } }), "ask");
  assert.equal(rowStatus({ chat: { pending: 1, waiting_for: "permission" } }), "permit");
  assert.equal(rowStatus({ waiting: true, busy: true }), "ask"); // session du terminal
  assert.equal(rowStatus({ busy: true, monitor: true }), "working");
  assert.equal(rowStatus({ monitor: true }), "monitor");
  assert.equal(rowStatus({}), "done");
  assert.equal(rowStatus({ chat: { status: "idle", unread: true } }), "unread");
  assert.equal(rowStatus({ chat: { status: "exited" } }), "done");
  assert.equal(rowStatus({ chat: { status: "idle" } }), "idle");
});

const qs = [
  { question: "Couleur ?", options: [{ label: "Bleu" }, { label: "Rouge" }] },
  { question: "Plats ?", multiSelect: true, options: [{ label: "Pizza" }, { label: "Sushi" }] },
];

test("questions : choix unique, multiple, texte libre, onglet suivant", () => {
  const m = askModel(qs);
  assert.equal(m.nextTab(-1), 0);
  assert.equal(m.choose(0, 1), true); // choix unique : question close
  assert.equal(m.value(0), "Rouge");
  assert.equal(m.nextTab(0), 1);
  assert.equal(m.choose(1, 0), false); // multiple : reste ouverte
  m.choose(1, 1);
  m.setOther(1, " Couscous ");
  assert.equal(m.value(1), "Pizza, Sushi, Couscous");
  m.choose(1, 0); // décoche
  assert.equal(m.value(1), "Sushi, Couscous");
  m.setOther(0, "Vert"); // texte libre d'un choix unique : remplace le choix
  assert.equal(m.value(0), "Vert");
  assert.equal(m.nextTab(0), 2); // tout est répondu : récapitulatif
  assert.deepEqual(m.answers(), { "Couleur ?": "Vert", "Plats ?": "Sushi, Couscous" });
  assert.equal(m.choose(0, 9), false); // option inexistante
});

test("commande ! : méta, mot de passe, sortie bornée, échappement", () => {
  assert.equal(bashMeta({ code: 0, ms: 250 }), "250 ms");
  assert.equal(bashMeta({ code: 3, ms: 1500 }), "code 3 · 1.5 s");
  assert.equal(bashMeta({ stopped: true }), "arrêtée");
  assert.equal(bashMeta({ timed_out: true }), "arrêtée : délai dépassé");
  assert.ok(asksPassword("Password:"));
  assert.ok(asksPassword("[sudo] password for yann: "));
  assert.ok(asksPassword("Mot de passe : "));
  assert.ok(!asksPassword("password changed\n"));
  assert.equal(appendTail("abc", "def", 4), "…cdef");
  const row = bashRow({ id: "b1", command: "<x>" }, true);
  assert.ok(row.includes("&lt;x&gt;") && row.includes("bash-stop") && row.includes("bash-in"));
  assert.ok(!bashRow({ id: "b1", command: "ls", code: 0, ms: 1, stdout: "a" }).includes("bash-in"));
});

test("plan : choix selon le mode, motif, issue", () => {
  assert.deepEqual(planChoices(true).map((c) => c.mode), [null]); // bypass : y revenir
  assert.deepEqual(planChoices(false).map((c) => c.mode), ["acceptEdits", null]);
  assert.match(keepPlanningMessage(" plus de tests "), /feedback: plus de tests$/);
  assert.match(keepPlanningMessage(""), /keep planning/);
  assert.equal(planOutcome({ allow: true }, planChoices(true)), "Plan approuvé.");
  assert.equal(planOutcome({ allow: true, mode: "acceptEdits" }, planChoices(false)), "Plan approuvé : modifications acceptées d'office.");
  assert.equal(planOutcome({ allow: false, message: keepPlanningMessage("X") }, planChoices(true)), "Claude continue à planifier, avec ta remarque : « X »");
});
