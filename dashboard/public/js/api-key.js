// Geteiltes Operator-API-Key-Modul fuer das Dashboard-Frontend.
//
// Der Key wird ausschliesslich In-Memory gehalten (modul-globale Variable, eine
// Instanz pro geladener Seite) und view-uebergreifend von control.js, floorplan.js
// und timetravel.js genutzt. Ersetzt die frueher in jeder dieser Dateien duplizierte
// sessionStorage-Logik.
//
// WICHTIG (Ehrlichkeit, kein Security-Theater): In-Memory behebt den CodeQL-Alert
// `js/clear-text-storage-of-sensitive-data` und die Duplikation — es ist KEINE echte
// XSS-Haertung. Gegen einen Angreifer, der JS im selben Kontext ausfuehrt (XSS), ist
// diese Variable genauso lesbar wie sessionStorage. Echter Token-Theft-Schutz (Key gar
// nicht im JS-zugaenglichen Storage halten, z.B. httpOnly-Cookie / server-side Auth)
// ist als separates Folge-Issue getrackt.
//
// Trade-off gegenueber sessionStorage: der Key ueberlebt KEINEN Seiten-Reload (F5) —
// fuer ein Operator-Tool verschmerzbar (Eingabe einmal pro Session).

let apiKey = "";

export function getApiKey() {
  return apiKey;
}

export function setApiKey(value) {
  apiKey = (value || "").trim();
}

export function authHeaders() {
  return apiKey ? { Authorization: "Bearer " + apiKey } : {};
}
