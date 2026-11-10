// État partagé entre la barre latérale et le détail : projets, comptes,
// discussions en direct (relues toutes les 2 s) et conversations par projet.
import { api } from "./lib.js";

export const store = {
  repos: [],
  accounts: [],
  chats: [],
  convs: new Map(), // id de projet → conversations (les plus récentes d'abord)
};

const listeners = new Set();
/** `fn()` est rappelé quand les projets ou les discussions en direct changent. */
export function subscribe(fn) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}
const notify = () => listeners.forEach((fn) => fn());

let loaded = null;
export function ensureLoaded() {
  if (!loaded) {
    loaded = Promise.all([api("/api/repos"), api("/api/accounts"), api("/api/chats").catch(() => [])]).then(([r, a, c]) => {
      store.repos = r;
      store.accounts = a;
      store.chats = c;
    });
  }
  return loaded;
}

/** Conversations d'un projet (mises en cache jusqu'à la prochaine analyse). */
export async function convsFor(repoId) {
  if (!store.convs.has(repoId)) store.convs.set(repoId, api(`/api/repos/${repoId}/conversations?sort=recent`).catch(() => []));
  return store.convs.get(repoId);
}

/** Après une analyse ou un réglage de projet : tout se relit. */
export async function reload() {
  store.convs.clear();
  loaded = null;
  await ensureLoaded();
  notify();
}

export function setRepos(repos) {
  store.repos = repos;
  notify();
}

// Discussions en direct : poussées par le serveur (flux /api/events) à chaque changement ;
// signalées seulement si ce que la barre affiche change. Le navigateur se reconnecte seul.
const sig = (l) => JSON.stringify(l.map((c) => [c.id, c.name, c.status, c.unread, c.pending, c.session_id, c.model, c.cost_usd, c.tokens_total, c.queue?.length]));
const take = (chats) => {
  if (!chats || sig(chats) === sig(store.chats)) return;
  store.chats = chats;
  notify();
};
new EventSource("/api/events").addEventListener("chats", (e) => { try { take(JSON.parse(e.data)); } catch {} });
