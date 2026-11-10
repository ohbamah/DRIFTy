// Confirmations et saisies courtes dans l'interface, à la place de `confirm()` et
// `prompt()` : dans l'app macOS (WKWebView), ceux du navigateur ne s'affichent pas et
// répondent « non » sans rien montrer, ce qui faisait échouer l'action sans un mot.
import { esc } from "./lib.js";

function open(html, onReady) {
  return new Promise((resolve) => {
    const d = document.createElement("dialog");
    d.className = "ask-dlg";
    d.innerHTML = `<form class="dlg" method="dialog">${html}</form>`;
    document.body.append(d);
    d.addEventListener("close", () => { resolve(d.returnValue); d.remove(); });
    d.showModal();
    onReady?.(d);
  });
}

/** Question oui / non ; `danger` colore le bouton de confirmation. */
export async function confirmBox(message, { ok = "Confirmer", cancel = "Annuler", danger = false } = {}) {
  const [first, ...rest] = String(message).split("\n\n");
  const v = await open(`<h2 class="ask-t">${esc(first)}</h2>${rest.length ? `<div class="ask-b">${esc(rest.join("\n\n"))}</div>` : ""}
    <div class="foot"><button class="btn" value="no">${esc(cancel)}</button><button class="btn ${danger ? "danger" : "primary"}" value="yes" autofocus>${esc(ok)}</button></div>`);
  return v === "yes";
}

/** Saisie d'une ligne ; `null` si annulée. */
export async function promptBox(message, value = "", { ok = "Valider" } = {}) {
  let input;
  const v = await open(`<h2 class="ask-t">${esc(message)}</h2><input class="field" value="${esc(value)}" maxlength="120">
    <div class="foot"><button class="btn" value="no" formnovalidate>Annuler</button><button class="btn primary" value="yes">${esc(ok)}</button></div>`, (d) => {
    input = d.querySelector("input");
    input.focus();
    input.select();
  });
  return v === "yes" ? input.value : null;
}
