// Modèle d'un flow d'automatisation (sans DOM, testé) : nœuds posés sur un plan, reliés de la
// sortie de l'un à l'entrée de l'autre. Le nœud Start est unique et fixe ; rien n'y entre, et
// les liens ne font jamais de boucle. Mêmes règles que le serveur (automations.rs).

/** Types de nœuds : libellé, icône (lib.js), couleur, et texte d'aperçu sur la carte. */
export const KINDS = {
  start: { label: "Start", icon: "play", tone: "var(--gain)", fixed: true, summary: () => "Lancement manuel" },
  github: { label: "GitHub", icon: "github", tone: "var(--ink-2)", summary: (c) => (c.action ? c.action : "Action à définir") },
  ai: { label: "IA", icon: "sparkle", tone: "var(--accent)", summary: (c) => c.prompt?.trim().split("\n")[0] || "Prompt à écrire" },
  bash: { label: "Commande bash", icon: "terminal", tone: "var(--bash)", summary: (c) => c.command?.trim().split("\n")[0] || "Commande à écrire" },
  webhook: { label: "Webhook", icon: "upload", tone: "var(--gold)", summary: (c) => (c.url ? `${c.method || "POST"} ${c.url}` : "URL à renseigner") },
};
/** Ceux qu'on glisse depuis la palette (le Start est déjà là). */
export const PALETTE = ["github", "ai", "bash", "webhook"];

/** Réglages de départ d'un nœud. */
export function defaults(type) {
  return {
    start: { trigger: "manual" },
    github: { repo_id: null, action: "" },
    ai: { repo_id: null, slot: "perso", model: "sonnet", prompt: "" },
    bash: { repo_id: null, command: "", timeout_s: 600 },
    webhook: { method: "POST", url: "", headers: "", body: "" },
  }[type] || {};
}

const rid = () => Math.random().toString(36).slice(2, 10);

export function newFlow(name = "Nouveau flow") {
  return { id: "f" + rid(), name, nodes: [{ id: "start", type: "start", x: 80, y: 200, config: defaults("start") }], edges: [] };
}

export function addNode(flow, type, x, y) {
  if (!KINDS[type] || KINDS[type].fixed) throw new Error(`Un nœud « ${type} » ne peut pas être ajouté.`);
  const node = { id: type + "-" + rid(), type, x: Math.round(x), y: Math.round(y), config: defaults(type) };
  flow.nodes.push(node);
  return node;
}

export const nodeOf = (flow, id) => flow.nodes.find((n) => n.id === id);

/** `to` est-il atteignable depuis `from` en suivant les liens ? */
function reaches(flow, from, to) {
  const seen = new Set(), stack = [from];
  while (stack.length) {
    const n = stack.pop();
    if (n === to) return true;
    if (seen.has(n)) continue;
    seen.add(n);
    for (const e of flow.edges) if (e.from === n) stack.push(e.to);
  }
  return false;
}

/** Pourquoi le lien `from → to` est refusé, ou `null` s'il est possible. */
export function linkRefusal(flow, from, to) {
  const a = nodeOf(flow, from), b = nodeOf(flow, to);
  if (!a || !b) return "Ce nœud n'existe plus.";
  if (from === to) return "Un nœud ne peut pas se relier à lui-même.";
  if (KINDS[b.type].fixed) return "Rien ne peut entrer dans le Start.";
  if (flow.edges.some((e) => e.from === from && e.to === to)) return "Ces deux nœuds sont déjà reliés.";
  if (reaches(flow, to, from)) return "Ce lien ferait une boucle.";
  return null;
}

export function connect(flow, from, to) {
  const why = linkRefusal(flow, from, to);
  if (!why) flow.edges.push({ from, to });
  return why;
}

export function removeNode(flow, id) {
  const n = nodeOf(flow, id);
  if (!n || KINDS[n.type].fixed) return false;
  flow.nodes = flow.nodes.filter((x) => x.id !== id);
  flow.edges = flow.edges.filter((e) => e.from !== id && e.to !== id);
  return true;
}

export function removeEdge(flow, from, to) {
  flow.edges = flow.edges.filter((e) => !(e.from === from && e.to === to));
}

/** Tracé d'un lien : courbe de la sortie (à droite de `a`) vers l'entrée (à gauche de `b`). */
export function edgePath(ax, ay, bx, by) {
  const d = Math.max(40, Math.abs(bx - ax) / 2);
  return `M${ax},${ay} C${ax + d},${ay} ${bx - d},${by} ${bx},${by}`;
}
