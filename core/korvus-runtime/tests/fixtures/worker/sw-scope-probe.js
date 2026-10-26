// SSOT W4.i: report the ServiceWorkerGlobalScope's own shape back to the page over the
// `sw-status` BroadcastChannel. `clients`, `registration` and `skipWaiting` were an object literal
// and an arrow in the kernel's `SW_PRELUDE` string; they are `ServiceWorkerGlobalScope` table rows
// with Rust bodies now, and this is what the worker realm sees of them.
const ts = Function.prototype.toString;
const nat = (f) => { try { return ts.call(f).includes("[native code]"); } catch (e) { return false; } };
self.addEventListener("activate", async () => {
  const out = [];
  out.push("skipWaitingNative=" + nat(self.skipWaiting));
  out.push("skipWaitingLen=" + self.skipWaiting.length);
  out.push("skipWaitingResolves=" + String(await self.skipWaiting()));
  // The four handler rows: accessors of the realm's event-handler store, NOT the plain `null` data
  // properties the prelude wrote over them.
  out.push("handlers=" + ["oninstall", "onactivate", "onfetch", "onmessage"].map((n) => {
    let o = self, d = null;
    while (o && !d) { d = Object.getOwnPropertyDescriptor(o, n); o = Object.getPrototypeOf(o); }
    return d ? (typeof d.get === "function" ? "accessor" : "data") : "absent";
  }).join(","));
  // Setting one and reading it back proves the store is live.
  const f = () => {};
  self.onfetch = f;
  out.push("handlerRoundTrip=" + (self.onfetch === f));
  out.push("registrationTag=" + Object.prototype.toString.call(self.registration));
  out.push("registrationInstance=" + (self.registration instanceof ServiceWorkerRegistration));
  out.push("registrationOwn=" + JSON.stringify(Object.getOwnPropertyNames(self.registration)));
  out.push("registrationScope=" + self.registration.scope);
  out.push("registrationStates=" + String(self.registration.installing) + "/" + String(self.registration.waiting) + "/" + String(self.registration.active));
  out.push("registrationUnregister=" + String(await self.registration.unregister()));
  // The literal's `addEventListener` was `() => {}` and swallowed every listener; a real
  // registration inherits EventTarget's.
  out.push("registrationListener=" + (self.registration.addEventListener === EventTarget.prototype.addEventListener));
  out.push("clientsNative=" + [self.clients.claim, self.clients.matchAll, self.clients.get].map(nat).join(","));
  out.push("clientsSame=" + (self.clients === self.clients));
  const cs = await self.clients.matchAll();
  // The id is a UUID, made once per worker, as Chrome 150's is (TEMPLATES P6 B22) -- its SHAPE, not its value.
  out.push("client=" + cs.length + ":" + /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(cs[0].id) + ":" + cs[0].type + ":" + nat(cs[0].postMessage));
  // `[Global]` puts a derived scope's members ON THE GLOBAL OBJECT, which is how
  // `install_worker_global_scope` (W6.2) mounts every one of them -- `DedicatedWorkerGlobalScope`'s
  // `onmessage` is an own property of a dedicated worker's global for the same reason. The prelude's
  // `globalThis.clients = {...}` produced own properties too, so this is unchanged by the cutover;
  // it is asserted so that a future change to the `[Global]` mount is visible here.
  out.push("ownKeys=" + ["clients", "registration", "skipWaiting"].map((n) =>
      Object.prototype.hasOwnProperty.call(self, n)).join(","));
  new BroadcastChannel("sw-status").postMessage(out.join("|"));
});
