// GOAL-4 W5.1 ServiceWorker fixture: signal the page over a BroadcastChannel as the SW runs its
// install -> activate lifecycle (proves a REAL SW isolate registered and drove its lifecycle).
self.addEventListener("install", () => { new BroadcastChannel("sw-status").postMessage("installed"); });
self.addEventListener("activate", () => { new BroadcastChannel("sw-status").postMessage("activated"); });
