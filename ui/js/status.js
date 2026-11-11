// Statut d'une ligne de conversation (barre latérale) : la règle, en un seul point.
//   ask     : Claude attend ta réponse (question ou plan) : carré bleu qui pulse ;
//   permit  : Claude attend une autorisation : carré rouge ;
//   working : Claude écrit ou réfléchit (même hors DRIFTy) : cercle orange ;
//   monitor : rien en cours, une surveillance attend sa notification : œil ;
//   puis unread / idle / done pour une discussion à l'arrêt.

/**
 * chat : discussion DRIFTy (ou null) ; busy / waiting : la session (transcript) est en plein
 * tour / attend une réponse à une question ou un plan ; monitor : surveillance en cours.
 */
export function rowStatus({ chat = null, busy = false, waiting = false, monitor = false } = {}) {
  if (chat?.pending) return chat.waiting_for === "permission" ? "permit" : "ask";
  if (waiting) return "ask";
  if (chat?.status === "working" || busy) return "working";
  if (monitor) return "monitor";
  if (!chat) return "done";
  return chat.unread ? "unread" : chat.status === "exited" ? "done" : "idle";
}
