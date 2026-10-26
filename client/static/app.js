// The stand-alone client korvus serves at /client/: one <korvus-browser> filling the window, connected with the
// ticket `korvus serve` printed.
import "./korvus-client.js";

// The ticket `korvus serve` printed travels in the fragment, which never reaches a server; it is kept for this
// tab's reloads and taken out of the address bar.
const hash = new URLSearchParams(location.hash.slice(1));
if (hash.get("ticket")) {
  sessionStorage.setItem("korvus-ticket", hash.get("ticket"));
  if (hash.get("persona") !== null) sessionStorage.setItem("korvus-persona", hash.get("persona"));
  history.replaceState(null, "", location.pathname);
}
const ticket = sessionStorage.getItem("korvus-ticket");
const persona = sessionStorage.getItem("korvus-persona") || "";
if (!ticket) {
  const p = document.createElement("p");
  p.className = "missing";
  p.textContent = "Open the client link korvus serve printed when it started. It carries the ticket this page connects with.";
  document.body.append(p);
} else {
  const el = document.createElement("korvus-browser");
  const scheme = location.protocol === "https:" ? "wss" : "ws";
  const route = persona ? `/persona/${encodeURIComponent(persona)}` : "";
  el.setAttribute("endpoint", `${scheme}://${location.host}${route}/?ticket=${encodeURIComponent(ticket)}`);
  if (persona) el.setAttribute("persona", persona);
  document.body.append(el);
}
