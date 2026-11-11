// Surveillance de la page : un gel ou une erreur laissent une trace dans le journal du démon
// (avant, un gel de la page ne laissait rien : il fallait l'échantillonner à la main).
// - Gel PASSAGER : le minuteur mesure son propre retard ; au-delà de 2 s, la page était
//   bloquée et le dit dès qu'elle reprend la main (durée et vue affichée).
// - Gel DURABLE : tant qu'elle est visible, la page fait signe toutes les 5 s ; si les signes
//   cessent sans qu'elle ait dit se cacher ou se fermer, le démon note un gel probable.
// - Erreurs JavaScript et promesses rejetées sans traitement.
const BEAT_MS = 5000, TICK_MS = 500, BLOCKED_MS = 2000;
const id = Math.random().toString(36).slice(2, 10);

const post = (path, body) => {
  try {
    const blob = new Blob([JSON.stringify({ id, ...body })], { type: "application/json" });
    // `sendBeacon` part même pendant la fermeture de la page.
    if (!navigator.sendBeacon?.(path, blob)) fetch(path, { method: "POST", body: blob, keepalive: true }).catch(() => {});
  } catch {}
};
const view = () => location.hash.split("?")[0] || "#/";
const log = (kind, message, extra = {}) => post("/api/client/log", { kind, message: String(message).slice(0, 2000), view: view(), ...extra });

export function startWatchdog() {
  let last = performance.now(), lastBeat = 0;
  setInterval(() => {
    const now = performance.now(), late = now - last - TICK_MS;
    last = now;
    // Une page cachée a ses minuteurs ralentis par le navigateur : ce n'est pas un gel.
    if (late > BLOCKED_MS && !document.hidden) log("blocked", `page bloquée ${Math.round(late)} ms`, { ms: Math.round(late) });
    if (!document.hidden && now - lastBeat >= BEAT_MS) { lastBeat = now; post("/api/client/beat", { visible: true }); }
  }, TICK_MS);
  document.addEventListener("visibilitychange", () => post("/api/client/beat", { visible: !document.hidden }));
  addEventListener("pagehide", () => post("/api/client/beat", { visible: false }));
  addEventListener("error", (e) => {
    // Avertissement bénin du navigateur (un ResizeObserver qui relance une mise en page), pas une erreur.
    if (/ResizeObserver loop/.test(e.message)) return;
    log("error", e.message, { stack: e.error?.stack?.slice(0, 4000), at: `${e.filename}:${e.lineno}:${e.colno}` });
  });
  addEventListener("unhandledrejection", (e) => log("rejection", e.reason?.message || e.reason, { stack: e.reason?.stack?.slice(0, 4000) }));
  post("/api/client/beat", { visible: !document.hidden });
}
