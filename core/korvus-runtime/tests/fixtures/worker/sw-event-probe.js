// SSOT W9.24: report the ServiceWorker realm's EVENT chain back to the page over the `sw-status`
// BroadcastChannel.
//
// `FetchEvent`, `ExtendableEvent` and `InstallEvent` were `SW_PRELUDE`, a JS string in
// `core/korvus-runtime/src/worker.rs` that decorated a plain `new Event('fetch')` with own `request`
// / `respondWith` / `waitUntil` properties. They are `interface!` tables with native constructors
// now, and this is what the SW realm sees of them. Every expectation in the matching test is
// MEASURED on real Chrome 150 inside a real ServiceWorker realm; see that test's doc comment.

const ts = Function.prototype.toString;
const nat = (f) => { try { return ts.call(f).includes("[native code]"); } catch (e) { return false; } };
const err = (f) => { try { const v = f(); return "no-throw:" + String(v); } catch (e) { return e.name + ": " + e.message; } };
const shape = (o, k) => {
  const d = Object.getOwnPropertyDescriptor(o, k);
  if (!d) return "absent";
  const kind = d.get ? "accessor" : "data";
  const fn = d.get || d.value;
  return kind + "/" + (d.enumerable ? "enum" : "non-enum") + "/" + (d.configurable ? "conf" : "non-conf") +
    "/" + (typeof fn === "function" ? (nat(fn) ? "native" : "SHIM") + ":" + fn.length : typeof fn);
};
const evShape = (e) => [
  e.constructor.name,
  Object.prototype.toString.call(e),
  JSON.stringify(Object.getOwnPropertyNames(e)),
  "trusted=" + e.isTrusted,
  "cancelable=" + e.cancelable,
  "bubbles=" + e.bubbles,
].join(":");

const seen = [];

self.addEventListener("install", (e) => {
  seen.push("install=" + evShape(e));
  // `waitUntil` was `() => {}` in the deleted string, so `activate` raced it. If it is real, the
  // `installWaitDone` line below lands BEFORE the `activate` line.
  e.waitUntil(new Promise((r) => setTimeout(() => { seen.push("installWaitDone"); r(); }, 20)));
});

self.addEventListener("activate", (e) => {
  seen.push("activate=" + evShape(e));
  // Claim the registering page first: only a controlled page's fetches reach this worker (Chrome 150,
  // REALWEB U21). A separate channel from the report one, so the page can fetch at the right moment
  // without racing the single `sw-status` message this fixture sends at the end.
  e.waitUntil(self.clients.claim());
  new BroadcastChannel("sw-ready").postMessage("go");
});

self.addEventListener("fetch", (e) => {
  const out = seen.slice();
  out.push("fetch=" + evShape(e));
  out.push("chain=" + (function (o) {
    const r = []; let p = Object.getPrototypeOf(o);
    while (p) { r.push(p.constructor && p.constructor.name); p = Object.getPrototypeOf(p); }
    return r.join(">");
  })(e));
  // The member ORDER of all three prototypes, `constructor` included (Chrome puts it last on each).
  for (const n of ["ExtendableEvent", "FetchEvent", "InstallEvent"]) {
    out.push(n + "=" + JSON.stringify(Object.getOwnPropertyNames(self[n].prototype)));
    out.push(n + ".ctor=" + self[n].name + "/" + self[n].length + "/" + nat(self[n]) +
      "/" + (Object.getPrototypeOf(self[n]).name || "Function.prototype") +
      "/" + (self[n].prototype[Symbol.toStringTag] || "none"));
  }
  out.push("waitUntilShape=" + shape(ExtendableEvent.prototype, "waitUntil"));
  out.push("respondWithShape=" + shape(FetchEvent.prototype, "respondWith"));
  out.push("requestShape=" + shape(FetchEvent.prototype, "request"));
  // `e.request` is a REAL `Request`, not the `{url, method:'GET'}` object literal the string passed
  // off as one.
  out.push("request=" + e.request.constructor.name + ":" + (e.request instanceof Request) +
    ":" + e.request.method + ":" + JSON.stringify(Object.getOwnPropertyNames(e.request)));
  out.push("requestUrlEndsWith=" + e.request.url.endsWith("/data"));
  out.push("ids=" + JSON.stringify([e.clientId, e.resultingClientId]) + ":" + e.isReload);
  out.push("promises=" + Object.prototype.toString.call(e.handled) + Object.prototype.toString.call(e.preloadResponse));
  // The receiver checks and the arity messages.
  out.push("foreignWait=" + err(() => ExtendableEvent.prototype.waitUntil.call({}, Promise.resolve())));
  out.push("foreignRespond=" + err(() => FetchEvent.prototype.respondWith.call({}, new Response("x"))));
  out.push("foreignGet=" + err(() => Object.getOwnPropertyDescriptor(FetchEvent.prototype, "request").get.call({})));
  out.push("noArgWait=" + err(() => e.waitUntil()));
  out.push("ctorNoInit=" + err(() => new FetchEvent("fetch")));
  out.push("ctorNoRequest=" + err(() => new FetchEvent("fetch", {})));
  out.push("ctorNoType=" + err(() => new ExtendableEvent()));
  out.push("ctorCall=" + err(() => FetchEvent("fetch", { request: new Request("/x") })));
  // A SCRIPT-CONSTRUCTED event: `waitUntil` refuses, `respondWith` does not, and it owns only
  // `isTrusted` like every other event instance (all measured on Chrome).
  const synth = new FetchEvent("fetch", { request: new Request("/x") });
  out.push("synth=" + evShape(synth));
  out.push("synthWait=" + err(() => synth.waitUntil(Promise.resolve())));
  out.push("synthRespond=" + err(() => { synth.respondWith(new Response("a")); return "ok"; }));
  out.push("synthRespondTwice=" + err(() => { synth.respondWith(new Response("b")); return "ok"; }));
  out.push("instanceOf=" + [e instanceof FetchEvent, e instanceof ExtendableEvent, e instanceof Event].join(","));

  e.respondWith(new Response("from-sw-event-probe"));
  // ...and only now the two dispatch-time InvalidStateErrors, which need the first call to have
  // happened.
  out.push("respondTwice=" + err(() => { e.respondWith(new Response("again")); return "ok"; }));
  setTimeout(() => {
    out.push("respondLate=" + err(() => { e.respondWith(new Response("late")); return "ok"; }));
    out.push("waitLate=" + err(() => { e.waitUntil(Promise.resolve()); return "ok"; }));
    new BroadcastChannel("sw-status").postMessage(out.join("|"));
  }, 0);
});
