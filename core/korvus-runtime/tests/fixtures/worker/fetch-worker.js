// GOAL-4 W5.2 fetch-interception fixture: intercept the page's fetch and serve a synthetic Response. Signal
// readiness on activate so the page fetches only after the fetch handler is wired (avoids a spawn race).
self.addEventListener("fetch", (e) => { e.respondWith(new Response("from-sw:" + e.request.url)); });
// `clients.claim()`: a page is controlled -- and its fetches reach this handler -- only once the worker
// claims it; registering does not do it (Chrome 150, REALWEB U21's `U-sw-control-hang` / `-claim`).
self.addEventListener("activate", (e) => { e.waitUntil(self.clients.claim()); new BroadcastChannel("sw-status").postMessage("ready"); });
